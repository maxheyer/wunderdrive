//! Tantivy full-text index wrapper (spec §6: BM25 + fuzzy, local and instant).
//!
//! Schema: `key` (the mirror-relative path, stored) + `text` (tokenized body).
//! One writer at a time, guarded by a [`TokioMutex`] — the engine triggers
//! sweeps after sync; concurrent triggers coalesce into one running sweep.

use std::path::PathBuf;
use std::sync::Arc;

use redb::Database;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, Schema, Value, STORED, STRING, TEXT};
use tantivy::snippet::{Snippet, SnippetGenerator};
use tantivy::{doc, Index as TvIndex, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument};
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, warn};

use crate::error::{Error, Result};
use crate::extract::extract_text;
use crate::journal::{self, local_for_key};

/// One search hit.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchHit {
    pub key: String,
    /// Up to ~160 chars around the best match, with `<mark>` tags.
    #[serde(default)]
    pub snippet: Option<String>,
}

pub struct Indexer {
    db: Arc<Database>,
    local_root: PathBuf,
    index: TvIndex,
    reader: IndexReader,
    writer: Arc<TokioMutex<IndexWriter>>,
    key_field: Field,
    text_field: Field,
}

impl Indexer {
    /// Open or create the index at `dir`. Schema is fixed; mismatches return an
    /// error (caller wipes + recreates if the on-disk schema ever changes).
    pub fn open(db: Arc<Database>, local_root: PathBuf, dir: &std::path::Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let mut schema_builder = Schema::builder();
        // STRING (not TEXT) so deletes by exact key term match cleanly — TEXT
        // would tokenize "gone.txt" into ["gone", "txt"] and break delete_term.
        let key_field = schema_builder.add_text_field("key", STRING | STORED);
        // STORED so the snippet generator can read the body back from the doc
        // store; TEXT for BM25 + fuzzy over the tokenized stream.
        let text_field = schema_builder.add_text_field("text", TEXT | STORED);
        let schema = schema_builder.build();

        let index = match TvIndex::open_in_dir(dir) {
            Ok(i) => {
                if i.schema() != schema {
                    // ponytail: schema bump = wipe + rebuild; trivial because the
                    // extraction cache lets us re-fill from existing hashes.
                    warn!("index schema changed; rebuilding");
                    std::fs::remove_dir_all(dir)?;
                    std::fs::create_dir_all(dir)?;
                    TvIndex::create_in_dir(dir, schema.clone())?
                } else {
                    i
                }
            }
            Err(_) => TvIndex::create_in_dir(dir, schema.clone())?,
        };

        // Default tokenizer ("default" = SimpleTokenizer + LowerCaser + Stemmer)
        // covers our Latin-case needs; no custom registration required.
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        let writer = index.writer(50 * 1024 * 1024)?; // 50 MiB heap
        Ok(Indexer {
            db,
            local_root,
            index,
            reader,
            writer: Arc::new(TokioMutex::new(writer)),
            key_field,
            text_field,
        })
    }

    /// Walk the journal and reconcile the Tantivy index against it.
    ///
    /// Three cases per key:
    /// - **Orphan** (in index, not in journal): delete the Tantivy doc. Catches
    ///   deletes and renames — a renamed file appears as a new key (re-indexed
    ///   below) + an orphaned old key (deleted here).
    /// - **New / hash changed** (in journal, not in index or hash differs):
    ///   extract (cache hit on rename), add a fresh Tantivy doc.
    /// - **Unchanged** (in both, same hash): skip.
    ///
    /// Returns the net number of docs added minus deleted.
    pub async fn sweep(&self) -> Result<usize> {
        let entries = journal::snapshot(&self.db)?;
        let indexed = journal::indexed_list(&self.db)?;
        let mut count: isize = 0;

        let mut writer = self.writer.lock().await;

        // Phase 1 — delete orphans (keys in the index but not in the journal).
        for key in indexed.keys() {
            if !entries.contains_key(key) {
                writer.delete_term(tantivy::Term::from_field_text(self.key_field, key));
                journal::indexed_remove(&self.db, key)?;
                count -= 1;
            }
        }

        // Phase 2 — add new / changed entries.
        for (key, entry) in &entries {
            // Skip if already indexed with the same content hash.
            if indexed.get(key) == Some(&entry.blake3) {
                continue;
            }
            // Cache hit? Reuse extracted text (free on rename / move). Otherwise
            // read the local file and extract.
            let text = if let Some(t) = journal::extract_get(&self.db, &entry.blake3)? {
                t
            } else {
                let path = local_for_key(&self.local_root, key);
                let t = extract_text(&path)?.unwrap_or_default();
                // ponytail: empty string is the "no extractable text" sentinel
                // so we don't re-parse binary files every sweep.
                journal::extract_put(&self.db, &entry.blake3, &t)?;
                t
            };
            // If a doc under this key already exists (hash changed), drop it
            // before adding the fresh one.
            writer.delete_term(tantivy::Term::from_field_text(self.key_field, key));
            if !text.is_empty() {
                writer.add_document(doc!(
                    self.key_field => key.clone(),
                    self.text_field => text,
                ))?;
                journal::indexed_put(&self.db, key, &entry.blake3)?;
                count += 1;
            } else {
                // No text to index; record the key as "seen" so we don't retry,
                // but leave no Tantivy doc behind.
                journal::indexed_remove(&self.db, key)?;
            }
        }

        writer.commit()?;
        self.reader.reload()?;
        debug!(net = count, "index sweep done");
        Ok(count.max(0) as usize)
    }

    /// Drop a document by key. Called when a file leaves the journal.
    pub async fn delete(&self, key: &str) -> Result<()> {
        let mut writer = self.writer.lock().await;
        writer.delete_term(tantivy::Term::from_field_text(self.key_field, key));
        writer.commit()?;
        self.reader.reload()?;
        journal::indexed_remove(&self.db, key)?;
        Ok(())
    }

    /// Wipe the Tantivy index + extraction cache + indexed manifest, then
    /// re-sweep from scratch. Recovery path for corruption or after a schema
    /// bump. Expensive — the next sweep re-extracts every file.
    pub async fn rebuild(&self) -> Result<usize> {
        let indexed = journal::indexed_list(&self.db)?;
        {
            let mut writer = self.writer.lock().await;
            for key in indexed.keys() {
                writer.delete_term(tantivy::Term::from_field_text(self.key_field, key));
            }
            writer.commit()?;
        }
        for key in indexed.keys() {
            journal::indexed_remove(&self.db, key)?;
        }
        journal::extract_clear(&self.db)?;
        self.reader.reload()?;
        self.sweep().await
    }

    /// Run a query, returning up to `limit` ranked hits with snippets.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let searcher = self.reader.searcher();
        let mut parser = QueryParser::for_index(&self.index, vec![self.text_field]);
        // ponytail: 1-edit fuzzy on each term catches most typos without
        // exploding the term space. Raise if precision suffers.
        parser.set_field_fuzzy(self.text_field, false, 1, true);
        let parsed = parser
            .parse_query(query)
            .map_err(|e| Error::other(format!("query parse: {e}")))?;

        let top = searcher
            .search(&parsed, &TopDocs::with_limit(limit))
            .map_err(|e| Error::other(format!("search: {e}")))?;

        // Snippet generator needs a non-fuzzy query: FuzzyTermQuery doesn't
        // implement query_terms, so the generator can't extract highlight
        // terms from a fuzzy parse. The snippet highlights exact-term matches
        // only — acceptable for a preview.
        let snippet_gen = QueryParser::for_index(&self.index, vec![self.text_field])
            .parse_query(query)
            .ok()
            .and_then(|q| SnippetGenerator::create(&searcher, &q, self.text_field).ok());

        let mut hits = Vec::with_capacity(top.len());
        for (_score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            let key = doc
                .get_first(self.key_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let snippet = snippet_gen
                .as_ref()
                .map(|g| g.snippet_from_doc(&doc))
                .map(|s: Snippet| s.to_html());
            hits.push(SearchHit { key, snippet });
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::JournalEntry;

    fn tmp_db() -> (tempfile::TempDir, Arc<Database>) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(journal::open(&dir.path().join("j.redb")).unwrap());
        (dir, db)
    }

    #[tokio::test]
    async fn sweep_indexes_text_files() {
        let (db_dir, db) = tmp_db();
        let root = tempfile::tempdir().unwrap();
        let f1 = root.path().join("a.txt");
        std::fs::write(&f1, b"the quick brown fox").unwrap();
        let f2 = root.path().join("b.txt");
        std::fs::write(&f2, b"lazy dogs in the sun").unwrap();
        let hash_a = crate::hash::hash_file(&f1).unwrap();
        let hash_b = crate::hash::hash_file(&f2).unwrap();
        journal::upsert(
            &db,
            "a.txt",
            &JournalEntry {
                blake3: hash_a,
                size: 19,
                mtime_millis: 1,
                remote_version: None,
            },
        )
        .unwrap();
        journal::upsert(
            &db,
            "b.txt",
            &JournalEntry {
                blake3: hash_b,
                size: 20,
                mtime_millis: 1,
                remote_version: None,
            },
        )
        .unwrap();

        let idx_dir = db_dir.path().join("idx");
        let indexer = Indexer::open(db.clone(), root.path().to_path_buf(), &idx_dir).unwrap();
        let n = indexer.sweep().await.unwrap();
        assert_eq!(n, 2);

        let hits = indexer.search("fox", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "a.txt");
        assert!(hits[0].snippet.as_deref().unwrap().contains("fox"));
    }

    #[tokio::test]
    async fn sweep_is_idempotent() {
        let (db_dir, db) = tmp_db();
        let root = tempfile::tempdir().unwrap();
        let f = root.path().join("only.txt");
        std::fs::write(&f, b"once more unto the breach").unwrap();
        let h = crate::hash::hash_file(&f).unwrap();
        journal::upsert(
            &db,
            "only.txt",
            &JournalEntry {
                blake3: h,
                size: 26,
                mtime_millis: 1,
                remote_version: None,
            },
        )
        .unwrap();
        let idx_dir = db_dir.path().join("idx");
        let indexer = Indexer::open(db.clone(), root.path().to_path_buf(), &idx_dir).unwrap();
        assert_eq!(indexer.sweep().await.unwrap(), 1);
        // Second sweep: cache hit, no new docs.
        assert_eq!(indexer.sweep().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn fuzzy_matches_typo() {
        let (db_dir, db) = tmp_db();
        let root = tempfile::tempdir().unwrap();
        let f = root.path().join("typo.txt");
        std::fs::write(&f, b"wunderdrive documentation").unwrap();
        let h = crate::hash::hash_file(&f).unwrap();
        journal::upsert(
            &db,
            "typo.txt",
            &JournalEntry {
                blake3: h,
                size: 26,
                mtime_millis: 1,
                remote_version: None,
            },
        )
        .unwrap();
        let idx_dir = db_dir.path().join("idx");
        let indexer = Indexer::open(db.clone(), root.path().to_path_buf(), &idx_dir).unwrap();
        indexer.sweep().await.unwrap();
        // "documentatoin" is one transposition away from "documentation".
        let hits = indexer.search("documentatoin", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "typo.txt");
    }

    #[tokio::test]
    async fn delete_removes_doc() {
        let (db_dir, db) = tmp_db();
        let root = tempfile::tempdir().unwrap();
        let f = root.path().join("gone.txt");
        std::fs::write(&f, b"ephemeral content here").unwrap();
        let h = crate::hash::hash_file(&f).unwrap();
        journal::upsert(
            &db,
            "gone.txt",
            &JournalEntry {
                blake3: h,
                size: 22,
                mtime_millis: 1,
                remote_version: None,
            },
        )
        .unwrap();
        let idx_dir = db_dir.path().join("idx");
        let indexer = Indexer::open(db.clone(), root.path().to_path_buf(), &idx_dir).unwrap();
        indexer.sweep().await.unwrap();
        indexer.delete("gone.txt").await.unwrap();
        assert!(indexer.search("ephemeral", 5).unwrap().is_empty());
    }

    #[tokio::test]
    async fn sweep_cleans_up_after_journal_delete() {
        // File gone from the journal (sync propagated the delete) but the
        // Tantivy doc lingers — sweep must reap it.
        let (db_dir, db) = tmp_db();
        let root = tempfile::tempdir().unwrap();
        let f = root.path().join("stale.txt");
        std::fs::write(&f, b"stale content lingers").unwrap();
        let h = crate::hash::hash_file(&f).unwrap();
        journal::upsert(
            &db,
            "stale.txt",
            &JournalEntry {
                blake3: h,
                size: 22,
                mtime_millis: 1,
                remote_version: None,
            },
        )
        .unwrap();
        let idx_dir = db_dir.path().join("idx");
        let indexer = Indexer::open(db.clone(), root.path().to_path_buf(), &idx_dir).unwrap();
        indexer.sweep().await.unwrap();
        assert_eq!(indexer.search("lingers", 5).unwrap().len(), 1);
        // Sync deletes the file: journal loses the entry, INDEXED_TABLE keeps it.
        journal::remove(&db, "stale.txt").unwrap();
        // Sweep should reap the orphan.
        indexer.sweep().await.unwrap();
        assert!(indexer.search("lingers", 5).unwrap().is_empty());
    }

    #[tokio::test]
    async fn sweep_handles_rename_via_cache() {
        // Rename: same content → same hash → cache hit. Old key should be
        // reaped from the index, new key added, no re-extraction.
        let (db_dir, db) = tmp_db();
        let root = tempfile::tempdir().unwrap();
        let f = root.path().join("old.txt");
        std::fs::write(&f, b"renamed content here").unwrap();
        let h = crate::hash::hash_file(&f).unwrap();
        journal::upsert(
            &db,
            "old.txt",
            &JournalEntry {
                blake3: h,
                size: 21,
                mtime_millis: 1,
                remote_version: None,
            },
        )
        .unwrap();
        let idx_dir = db_dir.path().join("idx");
        let indexer = Indexer::open(db.clone(), root.path().to_path_buf(), &idx_dir).unwrap();
        indexer.sweep().await.unwrap();
        assert_eq!(indexer.search("renamed", 5).unwrap()[0].key, "old.txt");
        // Rename in the journal: old key gone, new key added, same hash.
        journal::remove(&db, "old.txt").unwrap();
        journal::upsert(
            &db,
            "new.txt",
            &JournalEntry {
                blake3: h,
                size: 21,
                mtime_millis: 1,
                remote_version: None,
            },
        )
        .unwrap();
        indexer.sweep().await.unwrap();
        let hits = indexer.search("renamed", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "new.txt");
    }
}
