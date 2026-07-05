//! wunderdrive — terminal UI client.
//!
//! Connects to the `wunderdrive-daemon` over a local socket and renders the
//! mirror's state. Polls at ~10 Hz (local socket; imperceptible latency). Three
//! tabs: Files · Conflicts · Activity.

use std::io::stdout;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use futures::StreamExt;
use interprocess::local_socket::{
    tokio::{prelude::*, Stream},
    GenericNamespaced, Name, ToNsName,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs};
use ratatui::Terminal;
use tokio::io::BufStream;
use tokio::time::interval;
use wunderdrive_engine::protocol::{
    read_msg, write_msg, Request, Resolution, Response, METHOD_ACTIVITY, METHOD_INDEX_NOW,
    METHOD_PAUSE, METHOD_RESOLVE_CONFLICT, METHOD_RESUME, METHOD_SEARCH, METHOD_SNAPSHOT,
    METHOD_STATUS, METHOD_SYNC_NOW,
};
use wunderdrive_engine::{ActivityEntry, FileStatus, SearchHit, Snapshot, Status};

const TABS: [&str; 4] = ["Files", "Conflicts", "Activity", "Search"];

#[tokio::main]
async fn main() -> Result<()> {
    let socket = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "wunderdrive".to_string());
    let name = socket
        .as_str()
        .to_ns_name::<GenericNamespaced>()
        .with_context(|| format!("building socket name from {socket}"))?;

    let mut client = Client::connect(name).await.with_context(|| {
        format!("connecting to wunderdrive-daemon ({socket}). Is it running? Start it with `wunderdrive-daemon`")
    })?;

    // Initial fetch so the UI has something before the first render.
    let snapshot = client.snapshot().await.unwrap_or_default();
    let status = client.status().await.unwrap_or_default();
    let activity = client.activity(0).await.unwrap_or_default();

    let mut app = App {
        snapshot,
        status,
        activity,
        conflicts: Vec::new(),
        tab: 0,
        list: ListState::default(),
        search_query: String::new(),
        search_results: Vec::new(),
        last_error: None,
    };
    app.refresh_conflicts();

    setup_terminal()?;
    let result = run(&mut client, &mut app).await;
    restore_terminal();
    result
}

struct App {
    snapshot: Snapshot,
    status: Status,
    activity: Vec<ActivityEntry>,
    conflicts: Vec<String>,
    tab: usize,
    list: ListState,
    search_query: String,
    search_results: Vec<SearchHit>,
    last_error: Option<String>,
}

impl App {
    fn refresh_conflicts(&mut self) {
        self.conflicts = self
            .snapshot
            .files
            .iter()
            .filter(|f| f.status == FileStatus::Conflict)
            .map(|f| f.key.clone())
            .collect();
        if !self.conflicts.is_empty() && self.tab == 0 {
            // auto-switch to conflicts when they appear
        }
    }

    fn move_sel(&mut self, delta: i32) {
        let len = match self.tab {
            0 => self.snapshot.files.len(),
            1 => self.conflicts.len(),
            2 => self.activity.len(),
            _ => self.search_results.len(),
        };
        if len == 0 {
            self.list.select(None);
            return;
        }
        let i = self.list.selected().unwrap_or(0) as i32;
        let mut next = i + delta;
        if next < 0 {
            next = len as i32 - 1;
        }
        if next >= len as i32 {
            next = 0;
        }
        self.list.select(Some(next as usize));
    }

    /// In search tab, the input is always focused unless the user has cleared
    /// the query and run a search that returned nothing (ambiguous), so keep it
    /// simple: focused whenever we're on tab 3 and have either a query or no
    /// results to navigate.
    fn search_input_focused(&self) -> bool {
        self.tab == 3 && (self.search_query.is_empty() || self.search_results.is_empty())
    }
}

struct Client {
    stream: BufStream<Stream>,
    next_id: u64,
}

impl Client {
    async fn connect(name: Name<'_>) -> Result<Self> {
        let stream = Stream::connect(name)
            .await
            .context("connecting to daemon socket")?;
        Ok(Client {
            stream: BufStream::new(stream),
            next_id: 1,
        })
    }

    async fn call(&mut self, method: &str, params: serde_json::Value) -> Result<Response> {
        let id = self.next_id;
        self.next_id += 1;
        let req = Request {
            id,
            method: method.to_string(),
            params,
        };
        write_msg(&mut self.stream, &req).await?;
        let resp: Response = read_msg(&mut self.stream)
            .await?
            .ok_or_else(|| anyhow!("daemon closed connection"))?;
        Ok(resp)
    }

    async fn snapshot(&mut self) -> Result<Snapshot> {
        let r = self.call(METHOD_SNAPSHOT, serde_json::Value::Null).await?;
        parse(r)
    }
    async fn status(&mut self) -> Result<Status> {
        let r = self.call(METHOD_STATUS, serde_json::Value::Null).await?;
        parse(r)
    }
    async fn activity(&mut self, since: u64) -> Result<Vec<ActivityEntry>> {
        let r = self
            .call(METHOD_ACTIVITY, serde_json::json!({ "since": since }))
            .await?;
        parse(r)
    }
    async fn sync_now(&mut self) -> Result<()> {
        let _ = self.call(METHOD_SYNC_NOW, serde_json::Value::Null).await?;
        Ok(())
    }
    async fn pause(&mut self) -> Result<()> {
        let _ = self.call(METHOD_PAUSE, serde_json::Value::Null).await?;
        Ok(())
    }
    async fn resume(&mut self) -> Result<()> {
        let _ = self.call(METHOD_RESUME, serde_json::Value::Null).await?;
        Ok(())
    }
    async fn resolve(&mut self, key: &str, res: Resolution) -> Result<()> {
        let _ = self
            .call(
                METHOD_RESOLVE_CONFLICT,
                serde_json::json!({ "key": key, "resolution": res }),
            )
            .await?;
        Ok(())
    }
    async fn search(&mut self, query: &str) -> Result<Vec<SearchHit>> {
        let r = self
            .call(
                METHOD_SEARCH,
                serde_json::json!({ "query": query, "limit": 100 }),
            )
            .await?;
        parse(r)
    }
    async fn index_now(&mut self) -> Result<()> {
        let _ = self.call(METHOD_INDEX_NOW, serde_json::Value::Null).await?;
        Ok(())
    }
}

fn parse<T: serde::de::DeserializeOwned>(r: Response) -> Result<T> {
    match r.result {
        Some(v) => serde_json::from_value(v).context("decoding daemon response"),
        None => Err(anyhow!("{}", r.error.unwrap_or_default())),
    }
}

async fn run(client: &mut Client, app: &mut App) -> Result<()> {
    let mut events = event::EventStream::new();
    let mut poll = interval(Duration::from_millis(100));
    let mut last_seq = app.activity.last().map(|e| e.seq).unwrap_or(0);

    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        tokio::select! {
            biased;
            maybe_ev = events.next() => {
                let Some(Ok(ev)) = maybe_ev else { continue; };
                if !handle_event(app, client, ev).await? { break; }
            }
            _ = poll.tick() => {
                if let Ok(s) = client.snapshot().await {
                    app.snapshot = s;
                    app.refresh_conflicts();
                }
                if let Ok(st) = client.status().await { app.status = st; }
                if let Ok(mut new) = client.activity(last_seq).await {
                    if let Some(last) = new.last() { last_seq = last.seq; }
                    app.activity.extend(new.drain(..));
                    if app.activity.len() > 500 { let drop_n = app.activity.len() - 500; app.activity.drain(..drop_n); }
                }
            }
        }
    }
    Ok(())
}

/// Returns false to quit.
async fn handle_event(app: &mut App, client: &mut Client, ev: Event) -> Result<bool> {
    if let Event::Key(k) = ev {
        if k.kind != KeyEventKind::Press {
            return Ok(true);
        }

        // Search input captures all keys first; only Esc/Backspace/Enter/Up/Down
        // are interpreted as controls.
        if app.tab == 3 && app.search_input_focused() {
            match k.code {
                KeyCode::Esc => {
                    app.search_query.clear();
                    app.search_results.clear();
                    app.list.select(None);
                }
                KeyCode::Enter => {
                    if app.search_query.trim().is_empty() {
                        app.search_results.clear();
                    } else {
                        match client.search(&app.search_query).await {
                            Ok(hits) => {
                                app.search_results = hits;
                                app.list.select(if app.search_results.is_empty() {
                                    None
                                } else {
                                    Some(0)
                                });
                                app.last_error = None;
                            }
                            Err(e) => app.last_error = Some(e.to_string()),
                        }
                    }
                }
                KeyCode::Backspace => {
                    app.search_query.pop();
                }
                KeyCode::Down | KeyCode::Char('j') => app.move_sel(1),
                KeyCode::Up | KeyCode::Char('k') => app.move_sel(-1),
                KeyCode::Char(c) => app.search_query.push(c),
                _ => {}
            }
            return Ok(true);
        }

        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(false),
            KeyCode::Char('1') => app.tab = 0,
            KeyCode::Char('2') => app.tab = 1,
            KeyCode::Char('3') => app.tab = 2,
            KeyCode::Char('4') => app.tab = 3,
            KeyCode::Tab => app.tab = (app.tab + 1) % TABS.len(),
            KeyCode::BackTab => app.tab = (app.tab + TABS.len() - 1) % TABS.len(),
            KeyCode::Char('/') => {
                app.tab = 3;
                app.list.select(None);
            }
            KeyCode::Down | KeyCode::Char('j') => app.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => app.move_sel(-1),
            KeyCode::Char('r') => match client.sync_now().await {
                Ok(_) => app.last_error = None,
                Err(e) => app.last_error = Some(e.to_string()),
            },
            KeyCode::Char('p') => {
                let res = if app.status.paused {
                    client.resume().await
                } else {
                    client.pause().await
                };
                if let Err(e) = res {
                    app.last_error = Some(e.to_string());
                }
            }
            KeyCode::Char('i') => match client.index_now().await {
                Ok(_) => app.last_error = None,
                Err(e) => app.last_error = Some(e.to_string()),
            },
            KeyCode::Char('l') => {
                if app.tab == 1 {
                    if let Some(key) = app.conflicts.get(app.list.selected().unwrap_or(0)).cloned()
                    {
                        client.resolve(&key, Resolution::KeepLocal).await.ok();
                    }
                }
            }
            KeyCode::Char('o') => {
                if app.tab == 1 {
                    if let Some(key) = app.conflicts.get(app.list.selected().unwrap_or(0)).cloned()
                    {
                        client.resolve(&key, Resolution::KeepRemote).await.ok();
                    }
                }
            }
            KeyCode::Char('b') => {
                if app.tab == 1 {
                    if let Some(key) = app.conflicts.get(app.list.selected().unwrap_or(0)).cloned()
                    {
                        client.resolve(&key, Resolution::KeepBoth).await.ok();
                    }
                }
            }
            _ => {}
        }
    }
    Ok(true)
}

mod ui {
    use super::*;

    pub fn draw(f: &mut ratatui::Frame<'_>, app: &mut App) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(3),
            ])
            .split(f.area());

        let titles: Vec<Line> = TABS
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == app.tab {
                    Line::from(format!(" {t} "))
                } else {
                    Line::from(format!(" {t} "))
                }
            })
            .collect();
        let tabs = Tabs::new(titles)
            .block(Block::default().borders(Borders::ALL).title("wunderdrive"))
            .select(app.tab)
            .style(Style::default())
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Cyan),
            );
        f.render_widget(tabs, chunks[0]);

        match app.tab {
            0 => draw_files(f, app, chunks[1]),
            1 => draw_conflicts(f, app, chunks[1]),
            2 => draw_activity(f, app, chunks[1]),
            _ => draw_search(f, app, chunks[1]),
        }

        let mut help = format!(
            " [1-4] tabs  ↑↓ move  [r] sync-now  [i] index-now  [p] {pause}  [/] search  [q] quit",
            pause = if app.status.paused { "resume" } else { "pause" }
        );
        if app.tab == 1 {
            help.push_str("  [l] keep-local  [o] keep-remote  [b] keep-both");
        }
        if app.tab == 3 {
            help.push_str("  [enter] run  [esc] clear");
        }
        if let Some(e) = &app.last_error {
            help.push_str(&format!("   ⚠ {e}"));
        }
        let status = Paragraph::new(help).block(Block::default().borders(Borders::ALL));
        f.render_widget(status, chunks[2]);
    }

    fn draw_files(f: &mut ratatui::Frame<'_>, app: &mut App, area: ratatui::layout::Rect) {
        let items: Vec<ListItem> = app
            .snapshot
            .files
            .iter()
            .map(|f| {
                let mark = match f.status {
                    FileStatus::Synced => Span::raw("  "),
                    FileStatus::PendingUpload => {
                        Span::styled("↑ ", Style::default().fg(Color::Yellow))
                    }
                    FileStatus::NewLocal => Span::styled("+ ", Style::default().fg(Color::Green)),
                    FileStatus::DeletedPending => {
                        Span::styled("✗ ", Style::default().fg(Color::Red))
                    }
                    FileStatus::Conflict => Span::styled("! ", Style::default().fg(Color::Magenta)),
                };
                ListItem::new(Line::from(vec![
                    mark,
                    Span::raw(human_size(f.size)),
                    Span::raw("  "),
                    Span::raw(f.key.clone()),
                ]))
            })
            .collect();
        let title = format!(
            " Files ({}) — {}   {}",
            app.snapshot.files.len(),
            app.status.bucket,
            if app.status.paused { "[PAUSED]" } else { "" }
        );
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        f.render_stateful_widget(list, area, &mut app.list);
    }

    fn draw_conflicts(f: &mut ratatui::Frame<'_>, app: &mut App, area: ratatui::layout::Rect) {
        let items: Vec<ListItem> = if app.conflicts.is_empty() {
            vec![ListItem::new(Line::from(" No conflicts."))]
        } else {
            app.conflicts
                .iter()
                .map(|k| ListItem::new(Line::from(k.clone())))
                .collect()
        };
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" Conflicts ({}) ", app.conflicts.len())),
            )
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        f.render_stateful_widget(list, area, &mut app.list);
    }

    fn draw_activity(f: &mut ratatui::Frame<'_>, app: &mut App, area: ratatui::layout::Rect) {
        let items: Vec<ListItem> = app
            .activity
            .iter()
            .rev()
            .map(|e| ListItem::new(Line::from(format!("[{}] {}", e.kind, e.key))))
            .collect();
        let list = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Activity ({}) ", app.activity.len())),
        );
        f.render_widget(list, area);
    }

    /// Search tab: input box at top, ranked results below with HTML-stripped
    /// snippets. Tantivy's snippet generator emits `<mark>…</mark>` tags; we
    /// strip them for the terminal (no rich highlighting yet).
    fn draw_search(f: &mut ratatui::Frame<'_>, app: &mut App, area: ratatui::layout::Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(1)])
            .split(area);

        let prompt = if app.search_query.is_empty() {
            "/ ".to_string()
        } else {
            format!("/ {}", app.search_query)
        };
        let cursor = if app.search_input_focused() { "_" } else { "" };
        let input = Paragraph::new(format!("{prompt}{cursor}"))
            .block(Block::default().borders(Borders::ALL).title(" Search "));
        f.render_widget(input, chunks[0]);

        let items: Vec<ListItem> = if app.search_results.is_empty() {
            if app.search_query.is_empty() {
                vec![ListItem::new(Line::from(
                    " Type a query and press Enter. Press [i] to (re)index now.",
                ))]
            } else {
                vec![ListItem::new(Line::from(" No matches."))]
            }
        } else {
            app.search_results
                .iter()
                .map(|h| {
                    let key = Line::from(vec![
                        Span::styled("› ", Style::default().fg(Color::Cyan)),
                        Span::raw(h.key.clone()),
                    ]);
                    let mut lines = vec![key];
                    if let Some(snip) = &h.snippet {
                        if !snip.is_empty() {
                            let plain = strip_html(snip);
                            lines.push(Line::from(format!("  {}", truncate_str(&plain, 120))));
                        }
                    }
                    ListItem::new(lines)
                })
                .collect()
        };
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" Results ({}) ", app.search_results.len())),
            )
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        f.render_stateful_widget(list, chunks[1], &mut app.list);
    }

    fn human_size(n: u64) -> String {
        const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
        let mut f = n as f64;
        let mut i = 0;
        while f >= 1024.0 && i < UNITS.len() - 1 {
            f /= 1024.0;
            i += 1;
        }
        if i == 0 {
            format!("{n} B")
        } else {
            format!("{:.1} {}", f, UNITS[i])
        }
    }

    /// Strip the `<mark>…</mark>` tags Tantivy's snippet generator emits; we
    /// don't render rich highlighting in the terminal yet.
    fn strip_html(s: &str) -> String {
        s.replace("<mark>", "").replace("</mark>", "")
    }

    /// Truncate to `max` chars on a word boundary, appending "…".
    fn truncate_str(s: &str, max: usize) -> String {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() <= max {
            return s.to_string();
        }
        let mut end = max.saturating_sub(1);
        while end > 0 && !chars[end].is_whitespace() {
            end -= 1;
        }
        if end == 0 {
            end = max.saturating_sub(1);
        }
        let head: String = chars[..end].iter().collect();
        format!("{head}…")
    }
}

fn setup_terminal() -> Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableMouseCapture)?;
    Ok(())
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = stdout().execute(LeaveAlternateScreen);
    let _ = stdout().execute(DisableMouseCapture);
}
