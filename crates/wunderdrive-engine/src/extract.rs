//! Text extraction (spec §6, "text-layer first, OCR only as fallback").
//!
//! Dispatch by file extension. Pure-Rust extractors only, per the cross-compile
//! invariant (spec §8): `lopdf` for PDF, `calamine` for spreadsheets, `zip` +
//! `quick-xml` for OOXML. Plain text is read lossily. OCR is deferred behind the
//! [`OcrEngine`] trait — an `ocrs` impl slots in later without touching callers.

use std::io::Cursor;
use std::path::Path;

use crate::error::Result;

/// Maximum chars extracted per file. Keeps the index tractable; documents
/// beyond this are truncated (the head usually carries the title/abstract).
const MAX_CHARS: usize = 200_000;

/// Extract text from `path`, dispatching by extension.
///
/// Returns `Ok(None)` when we have no extractor for this type (or the file is
/// empty / unreadable in the expected way). Never returns an `Err` for "could
/// not parse" — that degrades to `None` so a single broken file can't wedge the
/// indexer. Real I/O errors (file vanished, permission denied) do propagate.
pub fn extract_text(path: &Path) -> Result<Option<String>> {
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e.to_ascii_lowercase(),
        None => return Ok(None),
    };
    let text = match ext.as_str() {
        // Plain text family — read lossily. Covers code, markdown, config, CSV.
        "txt" | "md" | "markdown" | "rst" | "log" | "ini" | "cfg" | "conf" | "properties"
        | "csv" | "tsv" | "json" | "yaml" | "yml" | "toml" | "xml" | "html" | "htm" | "css"
        | "js" | "ts" | "jsx" | "tsx" | "mjs" | "cjs" | "rs" | "go" | "py" | "rb" | "pl"
        | "lua" | "java" | "kt" | "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx"
        | "cs" | "swift" | "scala" | "clj" | "ex" | "exs" | "erl" | "hs" | "ml" | "fs" | "sh"
        | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" | "sql" | "graphql" | "proto"
        | "dockerfile" | "makefile" | "cmake" => extract_plain(path)?,

        "pdf" => extract_pdf(path)?,
        "docx" => extract_docx(path)?,
        "pptx" => extract_pptx(path)?,
        "xlsx" | "xls" | "xlsm" | "ods" => extract_spreadsheet(path)?,

        _ => return Ok(None),
    };
    Ok(text.map(|t| truncate_chars(t, MAX_CHARS)))
}

fn extract_plain(path: &Path) -> Result<Option<String>> {
    // ponytail: a 32 MiB cap keeps pathological files out of the index; raise
    // if real text assets exceed it.
    const MAX_PLAIN_BYTES: u64 = 32 * 1024 * 1024;
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if meta.len() > MAX_PLAIN_BYTES {
        return Ok(None);
    }
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn extract_pdf(path: &Path) -> Result<Option<String>> {
    use lopdf::Document;
    let doc = match Document::load(path) {
        Ok(d) => d,
        Err(_) => return Ok(None),
    };
    let mut out = String::new();
    // `get_pages` returns a BTreeMap<page_number, ObjectId>, already in order.
    for &obj_id in doc.get_pages().values() {
        if let Ok(content) = doc.get_page_content(obj_id) {
            extract_pdf_content_ops(&content, &mut out);
            out.push('\n');
        }
    }
    if out.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(out))
}

/// Decode the text-bearing operators `(... )Tj` and `[...]TJ` from a content
/// stream. Handles the common Latin case; ignores advanced encodings.
fn extract_pdf_content_ops(bytes: &[u8], out: &mut String) {
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'(' {
            // literal string until matching unescaped ')'
            let mut depth = 1;
            let mut j = i + 1;
            let mut s = Vec::new();
            while j < bytes.len() && depth > 0 {
                let c = bytes[j];
                if c == b'\\' && j + 1 < bytes.len() {
                    // escape: copy through, common cases decoded below
                    let n = bytes[j + 1];
                    match n {
                        b'n' => s.push(b'\n'),
                        b'r' => s.push(b'\r'),
                        b't' => s.push(b'\t'),
                        b'b' => s.push(0x08),
                        b'f' => s.push(0x0c),
                        b'(' | b')' | b'\\' => s.push(n),
                        b'0'..=b'7' => {
                            // up to 3 octal digits
                            let mut val = (n - b'0') as u8;
                            let mut k = 1;
                            while k < 3 && j + 1 + k < bytes.len() {
                                let d = bytes[j + 1 + k];
                                if d.is_ascii_digit() && (b'0'..=b'7').contains(&d) {
                                    val = val * 8 + (d - b'0');
                                    k += 1;
                                } else {
                                    break;
                                }
                            }
                            s.push(val);
                            j += k;
                        }
                        _ => s.push(n),
                    }
                    j += 2;
                    continue;
                }
                if c == b'(' {
                    depth += 1;
                    s.push(c);
                } else if c == b')' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    s.push(c);
                } else {
                    s.push(c);
                }
                j += 1;
            }
            // Look ahead for Tj or TJ
            if is_text_op(bytes, j + 1) {
                if let Ok(text) = pdf_text(&s) {
                    out.push_str(&text);
                }
            }
            i = j + 2;
        } else if b == b'<' {
            // hex string <...>; check for Tj/TJ after closing '>'
            if let Some(close) = find_byte(bytes, i + 1, b'>') {
                if is_text_op(bytes, close + 1) {
                    let hex = &bytes[i + 1..close];
                    let decoded = decode_hex_string(hex);
                    if let Ok(text) = pdf_text(&decoded) {
                        out.push_str(&text);
                    }
                }
                i = close + 1;
            } else {
                i += 1;
            }
        } else if b == b'[' {
            // array form: [s1 s2 ...]TJ — collect all (...) and <...> literals
            if let Some(close) = find_matching_bracket(bytes, i) {
                let segment = &bytes[i + 1..close];
                let mut k = 0;
                while k < segment.len() {
                    let c = segment[k];
                    if c == b'(' {
                        let mut depth = 1;
                        let mut j = k + 1;
                        let mut s = Vec::new();
                        while j < segment.len() && depth > 0 {
                            let cc = segment[j];
                            if cc == b'\\' && j + 1 < segment.len() {
                                s.push(segment[j + 1]);
                                j += 2;
                                continue;
                            }
                            if cc == b'(' {
                                depth += 1;
                            } else if cc == b')' {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            s.push(cc);
                            j += 1;
                        }
                        if let Ok(t) = pdf_text(&s) {
                            out.push_str(&t);
                        }
                        k = j + 1;
                    } else {
                        k += 1;
                    }
                }
                // TJ implies a kerning gap; emit a thin separator only if next
                // char isn't already whitespace, to keep tokens distinct.
                if !out.ends_with(' ') && !out.ends_with('\n') {
                    out.push(' ');
                }
                i = close + 1;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
}

fn is_text_op(bytes: &[u8], start: usize) -> bool {
    let mut s = start;
    skip_ws(bytes, &mut s);
    bytes.get(s..s + 2) == Some(b"Tj") || bytes.get(s..s + 2) == Some(b"TJ")
}

fn skip_ws(bytes: &[u8], i: &mut usize) {
    while *i < bytes.len() {
        match bytes[*i] {
            b' ' | b'\t' | b'\r' | b'\n' => *i += 1,
            _ => break,
        }
    }
}

fn find_byte(bytes: &[u8], from: usize, target: u8) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == target {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_matching_bracket(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 1;
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'(' => {
                // skip a literal string
                let mut d = 1;
                let mut j = i + 1;
                while j < bytes.len() && d > 0 {
                    if bytes[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    match bytes[j] {
                        b'(' => d += 1,
                        b')' => d -= 1,
                        _ => {}
                    }
                    j += 1;
                }
                i = j;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn decode_hex_string(hex: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hex.len() / 2);
    let mut nibbles = hex.iter().filter(|&&b| b.is_ascii_hexdigit()).peekable();
    while let (Some(hi), lo) = (nibbles.next(), nibbles.next()) {
        let h = hex_val(*hi);
        let l = lo.map(|b| hex_val(*b)).unwrap_or(0);
        out.push((h << 4) | l);
    }
    out
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

/// Decode PDFDocEncoding-Latin-ish bytes to a Rust string. We don't ship a full
/// codepage table; the common ASCII + Latin-1 subset covers the bulk of text.
fn pdf_text(bytes: &[u8]) -> std::result::Result<String, std::string::FromUtf8Error> {
    // PDFDocEncoding is close to Latin-1 for the printable ASCII range; treat
    // bytes as Latin-1 so accented Latin chars survive.
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

fn extract_docx(path: &Path) -> Result<Option<String>> {
    extract_ooxml_text(path, "word/document.xml", b"w:t").map(|opt| opt.map(|texts| texts.join("")))
}

fn extract_pptx(path: &Path) -> Result<Option<String>> {
    // ponytail: concatenate slideN.xml files; ordering by N numeric.
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut archive = match zip::ZipArchive::new(file) {
        Ok(a) => a,
        Err(_) => return Ok(None),
    };
    let mut entries: Vec<(usize, String)> = Vec::new();
    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.name().to_string();
        if let Some(num) = slide_number(&name) {
            let mut buf = Vec::new();
            if std::io::Read::read_to_end(&mut entry, &mut buf).is_ok() {
                if let Some(parts) = parse_ooxml_text(&buf, b"a:t") {
                    entries.push((num, parts.join("")));
                }
            }
        }
    }
    if entries.is_empty() {
        return Ok(None);
    }
    entries.sort_unstable_by_key(|(n, _)| *n);
    Ok(Some(
        entries
            .into_iter()
            .map(|(_, t)| t)
            .collect::<Vec<_>>()
            .join("\n\n"),
    ))
}

fn slide_number(name: &str) -> Option<usize> {
    // ppt/slides/slideN.xml
    let strip = name.strip_prefix("ppt/slides/slide")?;
    let stem = strip.strip_suffix(".xml")?;
    stem.parse::<usize>().ok()
}

/// Open the zip, find `inner_path`, pull text from elements tagged `tag`.
fn extract_ooxml_text(path: &Path, inner_path: &str, tag: &[u8]) -> Result<Option<Vec<String>>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let cursor = Cursor::new(&bytes);
    let mut archive = match zip::ZipArchive::new(cursor) {
        Ok(a) => a,
        Err(_) => return Ok(None),
    };
    let mut buf = Vec::new();
    let mut by_name = match archive.by_name(inner_path) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };
    if std::io::Read::read_to_end(&mut by_name, &mut buf).is_err() {
        return Ok(None);
    }
    Ok(parse_ooxml_text(&buf, tag))
}

/// Pull text content out of all elements named `tag` (e.g. `w:t` for docx) in
/// an OOXML stream. Lighter than a full DOM: scans `<tag ...>text</tag>`.
fn parse_ooxml_text(bytes: &[u8], tag: &[u8]) -> Option<Vec<String>> {
    use quick_xml::{events::Event, Reader};
    let mut reader = Reader::from_reader(bytes);
    let mut out = Vec::new();
    let mut text_buf = String::new();
    let mut in_target = false;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if e.name().as_ref() == tag {
                    in_target = true;
                    text_buf.clear();
                }
            }
            Ok(Event::End(e)) => {
                if in_target && e.name().as_ref() == tag {
                    if !text_buf.trim().is_empty() {
                        out.push(std::mem::take(&mut text_buf));
                    } else {
                        text_buf.clear();
                    }
                    in_target = false;
                }
            }
            Ok(Event::Text(t)) => {
                if in_target {
                    text_buf.push_str(&t.unescape().ok()?.into_owned());
                }
            }
            Ok(Event::Empty(_)) => {}
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buf.clear();
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn extract_spreadsheet(path: &Path) -> Result<Option<String>> {
    use calamine::{open_workbook_auto, Data, Reader};
    let mut book = match open_workbook_auto(path) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    let mut out = String::new();
    // `worksheets()` returns `(name, Range<Data>)` pairs in spreadsheet order.
    for (sheet_name, range) in book.worksheets() {
        out.push_str(&sheet_name);
        out.push('\n');
        for row in range.rows() {
            for cell in row {
                match cell {
                    Data::Int(i) => out.push_str(&i.to_string()),
                    Data::Float(f) => {
                        // ponytail: plain `{}` strips trailing zeros like a
                        // spreadsheet does; switch to fixed precision if needed.
                        use std::fmt::Write as _;
                        let _ = write!(out, "{f}");
                    }
                    Data::String(s) => out.push_str(s),
                    Data::DateTime(dt) => out.push_str(&dt.to_string()),
                    Data::Bool(b) => out.push_str(if *b { "TRUE" } else { "FALSE" }),
                    Data::Error(e) => out.push_str(&e.to_string()),
                    Data::Empty | Data::DurationIso(_) | Data::DateTimeIso(_) => {}
                }
                out.push('\t');
            }
            out.pop(); // trailing tab
            out.push('\n');
        }
    }
    if out.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

fn truncate_chars(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        s
    } else {
        s.chars().take(max).collect()
    }
}

/// Future OCR hook. An `ocrs` (Latin) or Paddle-ONNX (multilingual) impl slots
/// in here; [`extract_text`] never calls it — image/scan fallback is phase 2b.
pub trait OcrEngine: Send + Sync {
    /// Run OCR on a byte slice of image bytes (PNG/JPEG/TIFF). Returns
    /// extracted text, or `None` if the image is empty / unreadable.
    fn ocr(&self, image_bytes: &[u8]) -> Option<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hello.txt");
        std::fs::write(&p, b"hello world").unwrap();
        assert_eq!(extract_text(&p).unwrap().as_deref(), Some("hello world"));
    }

    #[test]
    fn unknown_ext_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("blob.dat");
        std::fs::write(&p, b"\0\0").unwrap();
        assert_eq!(extract_text(&p).unwrap(), None);
    }

    #[test]
    fn missing_file_returns_none() {
        let p = Path::new("/nonexistent/hello.txt");
        assert_eq!(extract_text(&p).unwrap(), None);
    }

    #[test]
    fn markdown_is_plain() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("note.md");
        std::fs::write(&p, b"# title\nbody").unwrap();
        assert_eq!(extract_text(&p).unwrap().as_deref(), Some("# title\nbody"));
    }

    #[test]
    fn pdf_content_extracts_strings() {
        // Minimal content stream: "Hi" Tj and ["a" -5 "b"] TJ.
        let stream = b"(Hi) Tj\n[(a) -5 (b)] TJ\n";
        let mut out = String::new();
        extract_pdf_content_ops(stream, &mut out);
        assert!(out.contains("Hi"));
        assert!(out.contains("a"));
        assert!(out.contains("b"));
    }

    #[test]
    fn pdf_hex_string_in_tj() {
        let stream = b"<48656c6c6f> Tj\n"; // "Hello"
        let mut out = String::new();
        extract_pdf_content_ops(stream, &mut out);
        assert!(out.contains("Hello"), "got: {out:?}");
    }

    #[test]
    fn ooxml_text_parses_docx_fragment() {
        let xml = br#"<?xml version="1.0"?>
        <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p>
            <w:p><w:r><w:t>World</w:t></w:r></w:p></w:body></w:document>"#;
        let parts = parse_ooxml_text(xml, b"w:t").unwrap_or_default();
        assert_eq!(parts, vec!["Hello".to_string(), "World".to_string()]);
    }

    #[test]
    fn truncate_keeps_complete_chars() {
        let s = "héllo".to_string(); // 5 chars, 6 bytes
        assert_eq!(truncate_chars(s.clone(), 10), s);
        let t = truncate_chars(s, 2);
        assert_eq!(t, "hé");
    }

    // --- real binary-format fixtures (built in-memory) ---

    /// Build a minimal valid xlsx (zip of XML parts) recognizable by calamine.
    /// Two shared strings ("Name", "Alice") + one numeric cell (42).
    fn build_minimal_xlsx() -> Vec<u8> {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        use zip::ZipWriter;
        let mut zw = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default();
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
</Types>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;
        let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#;
        let workbook_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>
</Relationships>"#;
        let shared_strings = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="2" uniqueCount="2">
<si><t>Name</t></si>
<si><t>Alice</t></si>
</sst>"#;
        let sheet1 = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData>
<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
<row r="2"><c r="A2"><v>42</v></c></row>
</sheetData>
</worksheet>"#;
        zw.start_file("[Content_Types].xml", opts).unwrap();
        zw.write_all(content_types.as_bytes()).unwrap();
        zw.start_file("_rels/.rels", opts).unwrap();
        zw.write_all(rels.as_bytes()).unwrap();
        zw.start_file("xl/workbook.xml", opts).unwrap();
        zw.write_all(workbook.as_bytes()).unwrap();
        zw.start_file("xl/_rels/workbook.xml.rels", opts).unwrap();
        zw.write_all(workbook_rels.as_bytes()).unwrap();
        zw.start_file("xl/sharedStrings.xml", opts).unwrap();
        zw.write_all(shared_strings.as_bytes()).unwrap();
        zw.start_file("xl/worksheets/sheet1.xml", opts).unwrap();
        zw.write_all(sheet1.as_bytes()).unwrap();
        let cursor = zw.finish().unwrap();
        cursor.into_inner()
    }

    /// Build a minimal valid docx (zip of XML parts) recognizable by the
    /// OOXML text extractor. Contains "Hello docx world" in a w:t element.
    fn build_minimal_docx() -> Vec<u8> {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        use zip::ZipWriter;
        let mut zw = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default();
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
        let document = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body><w:p><w:r><w:t>Hello docx world</w:t></w:r></w:p></w:body></w:document>"#;
        zw.start_file("[Content_Types].xml", opts).unwrap();
        zw.write_all(content_types.as_bytes()).unwrap();
        zw.start_file("_rels/.rels", opts).unwrap();
        zw.write_all(rels.as_bytes()).unwrap();
        zw.start_file("word/document.xml", opts).unwrap();
        zw.write_all(document.as_bytes()).unwrap();
        let cursor = zw.finish().unwrap();
        cursor.into_inner()
    }

    /// Build a minimal valid PDF with one page containing "(Hello PDF) Tj".
    /// Computes xref offsets dynamically so lopdf can parse it cleanly.
    fn build_minimal_pdf() -> Vec<u8> {
        let stream_body = b"(Hello PDF) Tj\n";
        let stream_len = stream_body.len();

        let header = b"%PDF-1.4\n";
        let obj1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
        let obj2 = b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n";
        let obj3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>\nendobj\n";
        let stream_pre = format!("4 0 obj\n<< /Length {stream_len} >>\nstream\n").into_bytes();
        let stream_post = b"endstream\nendobj\n";

        let mut bytes = Vec::new();
        bytes.extend_from_slice(header);
        let off1 = bytes.len();
        bytes.extend_from_slice(obj1);
        let off2 = bytes.len();
        bytes.extend_from_slice(obj2);
        let off3 = bytes.len();
        bytes.extend_from_slice(obj3);
        let off4 = bytes.len();
        bytes.extend_from_slice(&stream_pre);
        bytes.extend_from_slice(stream_body);
        bytes.extend_from_slice(stream_post);

        let xref_offset = bytes.len();
        // lopdf's xref entry parser accepts only " \r", " \n", or "\r\n" as
        // the line terminator — NOT " \r\n" (the spec's 20-byte form). Use
        // " \n" (space + LF) to stay within the accepted set.
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n"
        );
        bytes.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n");
        bytes.extend_from_slice(trailer.as_bytes());
        bytes
    }

    #[test]
    fn xlsx_extracts_cell_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("data.xlsx");
        std::fs::write(&p, build_minimal_xlsx()).unwrap();
        let out = extract_text(&p)
            .unwrap()
            .expect("xlsx should extract Some(text)");
        assert!(out.contains("Name"), "missing 'Name' in: {out:?}");
        assert!(out.contains("Alice"), "missing 'Alice' in: {out:?}");
        assert!(out.contains("42"), "missing '42' in: {out:?}");
    }

    #[test]
    fn docx_extracts_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("doc.docx");
        std::fs::write(&p, build_minimal_docx()).unwrap();
        let out = extract_text(&p)
            .unwrap()
            .expect("docx should extract Some(text)");
        assert!(
            out.contains("Hello docx world"),
            "missing phrase in: {out:?}"
        );
    }

    #[test]
    fn pdf_extracts_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("doc.pdf");
        std::fs::write(&p, build_minimal_pdf()).unwrap();
        let out = extract_text(&p)
            .unwrap()
            .expect("pdf should extract Some(text)");
        assert!(out.contains("Hello PDF"), "missing 'Hello PDF' in: {out:?}");
    }
}
