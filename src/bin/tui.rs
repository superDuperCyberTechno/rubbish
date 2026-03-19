// TUI for browsing dumps
// This file was renamed from tui_tui.rs to tui.rs
// (contents unchanged)

#![allow(clippy::needless_return)]

use std::io;
use std::io::Read;
use std::{fs, process::{Command, Stdio}, time::{SystemTime, Duration}, env};
// networking imports removed — server is launched as a child owned by the TUI
use std::path::PathBuf;
// mpsc types used directly where needed
use chrono::{DateTime, Local};
use crossterm::{execute, terminal::{enable_raw_mode, disable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}};
use crossterm::event::{self, Event as CEvent, KeyCode};
use signal_hook::consts::signal::*;
use signal_hook::iterator::Signals;
use std::sync::mpsc::channel;
use std::thread;
use std::collections::HashMap;
use atty::Stream;
use tui::backend::CrosstermBackend;
use tui::Terminal;
use tui::widgets::{Block, Borders, Paragraph, Wrap, Table, Row, Cell, Widget};
use tui::buffer::Buffer;
use tui::layout::Rect;
use tui::style::{Style, Modifier};
use tui::layout::{Layout, Constraint, Direction};
use tui::widgets::TableState;
use unicode_width::UnicodeWidthStr;

fn read_preview(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    // Read up to 64KB for preview and return the file contents as-is
    // (do not reformat/pretty-print JSON here; show the file text exactly as stored).
    let f = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(f);
    let mut buf = String::new();
    reader.take(64 * 1024).read_to_string(&mut buf)?;
    Ok(buf)
}

// A custom widget that renders raw preview text without any wrapping. Each line is truncated
// to the available width and written directly to the buffer, so there is no word-wrapping.
struct RawPreview<'a> {
    text: &'a str,
}

impl<'a> Widget for RawPreview<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut y = area.y as u16;
        let max_lines = area.height as usize;
        let max_width = area.width as usize;
        for (i, line) in self.text.lines().enumerate() {
            if i >= max_lines { break; }
            // normalize tabs
            let line = line.replace('\t', "    ");
            // truncate by displayed width (unicode-aware)
            let mut acc = String::new();
            let mut cur_w = 0usize;
            for ch in line.chars() {
                let cw = UnicodeWidthStr::width(ch.to_string().as_str());
                if cur_w + cw > max_width { break; }
                acc.push(ch);
                cur_w += cw;
            }
            // replace spaces with NBSPs so the paragraph renderer won't re-wrap
            let out = acc.replace(' ', "\u{00A0}");
            buf.set_stringn(area.x, y, &out, max_width, Style::default());
            y += 1;
        }
    }
}

fn human_size(bytes: u64) -> String {
    // Use IEC (binary) units: KiB, MiB, GiB, TiB, PiB (base 1024)
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KIB {
        format!("{} B", bytes)
    } else if b < KIB.powi(2) {
        format!("{:.1} KiB", b / KIB)
    } else if b < KIB.powi(3) {
        format!("{:.1} MiB", b / KIB.powi(2))
    } else if b < KIB.powi(4) {
        format!("{:.1} GiB", b / KIB.powi(3))
    } else if b < KIB.powi(5) {
        format!("{:.1} TiB", b / KIB.powi(4))
    } else {
        format!("{:.1} PiB", b / KIB.powi(5))
    }
}

// right_align is unused after removing the size column from the table

fn scan_dumps(dumps_dir: &std::path::Path) -> (Vec<(String, String, String)>, Vec<std::path::PathBuf>) {
    let mut files: Vec<(std::path::PathBuf, Option<SystemTime>, u64)> = Vec::new();
    if let Ok(rd) = fs::read_dir(dumps_dir) {
        files = rd
            .filter_map(|e| e.ok())
            .map(|e| {
                let path = e.path();
                if !path.is_file() {
                    return None;
                }
                let meta = path.metadata().ok();
                let mtime = meta.as_ref().and_then(|m| m.modified().ok());
                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                Some((path, mtime, size))
            })
            .filter_map(|x| x)
            .collect();
    }

    files.sort_by_key(|(_, mtime, _)| mtime.unwrap_or(SystemTime::UNIX_EPOCH));
    files.reverse();

    let mut entries: Vec<(String, String, String)> = Vec::new();
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    for (path, mtime, size) in files.iter() {
        let fname = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ts = mtime.as_ref().map(|t| DateTime::<Local>::from(*t).format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_else(|| "unknown".into());

        // filename format: [title]_[id].json -> extract title as everything before the last '_'
        let mut base = fname.clone();
        if base.ends_with(".json") {
            base.truncate(base.len() - 5);
        }
        let title = if let Some(idx) = base.rfind('_') {
            let t = &base[..idx];
            if t.is_empty() { "".to_string() } else { t.to_string() }
        } else {
            "".to_string()
        };

        let size_str = human_size(*size);
        entries.push((ts, title, size_str));
        paths.push(path.clone());
    }

    (entries, paths)
}

fn get_mtime(path: &std::path::Path) -> Option<SystemTime> {
    path.metadata().and_then(|m| m.modified()).ok()
}

// Build display entry (timestamp, title, size_str) and return mtime for sorting.
fn build_entry_from_path(path: &std::path::Path) -> Option<((String, String, String), SystemTime)> {
    if !path.is_file() {
        return None;
    }
    let meta = path.metadata().ok()?;
    let mtime = meta.modified().ok()?;
    let size = meta.len();

    let fname = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ts = DateTime::<Local>::from(mtime).format("%Y-%m-%d %H:%M:%S").to_string();

    // extract title from filename [title]_[id].json
    let mut base = fname.clone();
    if base.ends_with(".json") {
        base.truncate(base.len() - 5);
    }
    let title = if let Some(idx) = base.rfind('_') {
        let t = &base[..idx];
        if t.is_empty() { "".to_string() } else { t.to_string() }
    } else {
        "".to_string()
    };

    let size_str = human_size(size);
    Some(((ts, title, size_str), mtime))
}

fn apply_watch_event(ev: WatchEvent, dumps_dir: &std::path::Path, entries: &mut Vec<(String, String, String)>, paths: &mut Vec<std::path::PathBuf>, state: &mut TableState, preview: &mut String) {
    match ev {
        WatchEvent::Rescan => {
            let (new_entries, new_paths) = scan_dumps(dumps_dir);
            *entries = new_entries;
            *paths = new_paths;
            if entries.is_empty() {
                *preview = "(no preview available)".to_string();
                state.select(None);
            } else {
                // ensure selection is valid
                match state.selected() {
                    Some(i) if i < entries.len() => {
                        if let Some(p) = paths.get(i) {
                            *preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                        }
                    }
                    _ => {
                        state.select(Some(0));
                        if let Some(p) = paths.get(0) {
                            *preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                        }
                    }
                }
            }
        }
        WatchEvent::Created(p) | WatchEvent::Modified(p) => {
            // ignore events outside the dumps dir
            if !p.starts_with(dumps_dir) {
                return;
            }

            // if file exists, build entry
            if let Some((entry, mtime)) = build_entry_from_path(&p) {
                // find if path already exists in our list
                if let Some(pos) = paths.iter().position(|x| x == &p) {
                    // update existing entry
                    entries[pos] = entry;
                    // if this is the selected item, refresh preview
                    if state.selected() == Some(pos) {
                        *preview = read_preview(&p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                    }
                } else {
                    // determine insertion index based on mtime (newest first)
                    let mut inserted = false;
                    for (i, existing) in paths.iter().enumerate() {
                        let emtime = get_mtime(existing).unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        if mtime > emtime {
                            paths.insert(i, p.clone());
                            entries.insert(i, entry.clone());
                            // if we inserted before the selected index, shift selection down to keep same item
                            if let Some(sel) = state.selected() {
                                if i <= sel {
                                    state.select(Some(sel + 1));
                                }
                            }
                            inserted = true;
                            break;
                        }
                    }
                    if !inserted {
                        paths.push(p.clone());
                        entries.push(entry);
                    }
                    // if nothing selected, select the first item
                    if state.selected().is_none() {
                        state.select(Some(0));
                        if let Some(first) = paths.get(0) {
                            *preview = read_preview(first).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                        }
                    }
                }
            } else {
                // file missing or unreadable -> remove if present
                if let Some(pos) = paths.iter().position(|x| x == &p) {
                    paths.remove(pos);
                    entries.remove(pos);
                    // adjust selection
                    match state.selected() {
                        Some(sel) if sel == pos => {
                            if entries.is_empty() {
                                state.select(None);
                                *preview = "(no preview available)".to_string();
                            } else {
                                let new_sel = if pos == 0 { 0 } else { pos - 1 };
                                state.select(Some(new_sel));
                                if let Some(p2) = paths.get(new_sel) {
                                    *preview = read_preview(p2).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                                }
                            }
                        }
                        Some(sel) if sel > pos => {
                            state.select(Some(sel - 1));
                        }
                        _ => {}
                    }
                }
            }
        }
        WatchEvent::Removed(p) => {
            if let Some(pos) = paths.iter().position(|x| x == &p) {
                paths.remove(pos);
                entries.remove(pos);
                match state.selected() {
                    Some(sel) if sel == pos => {
                        if entries.is_empty() {
                            state.select(None);
                            *preview = "(no preview available)".to_string();
                        } else {
                            let new_sel = if pos == 0 { 0 } else { pos - 1 };
                            state.select(Some(new_sel));
                            if let Some(p2) = paths.get(new_sel) {
                                *preview = read_preview(p2).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                            }
                        }
                    }
                    Some(sel) if sel > pos => {
                        state.select(Some(sel - 1));
                    }
                    _ => {}
                }
            }
        }
    }
}

#[derive(Debug)]
enum WatchEvent {
    Created(std::path::PathBuf),
    Modified(std::path::PathBuf),
    Removed(std::path::PathBuf),
    Rescan,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // start the server as a child process owned by the TUI so it stops when the TUI exits.
    // Prefer a compiled binary in target/debug, otherwise use `cargo run --bin rubbish`.
    let server_child: Option<std::process::Child> = {
        // Try to open a server log file for stdout/stderr capture. Fall back to null if unavailable.
        let log_path = "server.log";
        let (stdout_dest, stderr_dest) = match fs::OpenOptions::new().create(true).append(true).open(log_path) {
            Ok(f) => match f.try_clone() {
                Ok(f2) => {
                    eprintln!("server logs -> {}", log_path);
                    (Stdio::from(f), Stdio::from(f2))
                }
                Err(_) => {
                    eprintln!("warning: failed to clone server log file; disabling logging");
                    (Stdio::null(), Stdio::null())
                }
            },
            Err(_) => {
                eprintln!("warning: failed to open server log file '{}'; logging disabled", log_path);
                (Stdio::null(), Stdio::null())
            }
        };

        // Try several candidate server executables in order before falling back to `cargo run`.
        let candidates = [
            "./target/debug/rubbish",
            "./target/release/rubbish",
            "./rubbish",
            "rubbish",
        ];
        let mut server_spawn = Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no candidate tried"));
        for c in &candidates {
            let cmd = std::path::Path::new(c);
            if !cmd.exists() {
                continue;
            }
            // open fresh log file handles for each spawn attempt
            match fs::OpenOptions::new().create(true).append(true).open(log_path) {
                Ok(f1) => {
                    match f1.try_clone() {
                        Ok(f2) => {
                            server_spawn = Command::new(c).stdout(Stdio::from(f1)).stderr(Stdio::from(f2)).spawn();
                        }
                        Err(_) => {
                            server_spawn = Command::new(c).stdout(Stdio::from(f1)).stderr(Stdio::null()).spawn();
                        }
                    }
                }
                Err(_) => {
                    server_spawn = Command::new(c).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
                }
            }
            if server_spawn.is_ok() { break; }
        }
        if server_spawn.is_err() {
            // final fallback: use `cargo run --bin rubbish` and attach the prepared dests
            server_spawn = Command::new("cargo").args(["run","--bin","rubbish"]).stdout(stdout_dest).stderr(stderr_dest).spawn();
        }

        let res = match server_spawn {
            Ok(child) => {
                let pid = child.id();
                eprintln!("started rubbish server (pid={})", pid);
                Some(child)
            }
            Err(e) => {
                eprintln!("failed to start rubbish server: {}", e);
                None
            }
        };

        // give server a moment to bind
        std::thread::sleep(Duration::from_millis(300));
        res
    };

    // determine dumps directory (XDG_DATA_HOME/rubbish/dumps or ~/.local/share/rubbish/dumps)
    let mut dumps_dir: PathBuf = match env::var("XDG_DATA_HOME") {
        Ok(x) if !x.is_empty() => PathBuf::from(x).join("rubbish").join("dumps"),
        _ => match env::var("HOME") {
            Ok(h) => PathBuf::from(h).join(".local").join("share").join("rubbish").join("dumps"),
            Err(_) => PathBuf::from("./dumps"),
        },
    };

    // Ensure dumps directory exists; if creation fails, fall back to ./dumps
    if let Err(e) = fs::create_dir_all(&dumps_dir) {
        eprintln!("warning: failed to create dumps dir {}: {}\nFalling back to ./dumps", dumps_dir.display(), e);
        dumps_dir = PathBuf::from("./dumps");
        let _ = fs::create_dir_all(&dumps_dir);
    }

    // collect files with metadata and sort by modified time (newest first)
    let mut files: Vec<(std::path::PathBuf, Option<SystemTime>, u64)>;
    if let Ok(rd) = fs::read_dir(&dumps_dir) {
        files = rd
            .filter_map(|e| e.ok())
            .map(|e| {
                let path = e.path();
                if !path.is_file() {
                    return None;
                }
                let meta = path.metadata().ok();
                let mtime = meta.as_ref().and_then(|m| m.modified().ok());
                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                Some((path, mtime, size))
            })
            .filter_map(|x| x)
            .collect();
    } else {
        // directory missing or unreadable — continue with empty files list so TUI still runs
        files = Vec::new();
    }

    // do not exit when there are no dumps; allow TUI to run with an empty list

    files.sort_by_key(|(_, mtime, _)| mtime.unwrap_or(SystemTime::UNIX_EPOCH));
    files.reverse();

    // entries: (timestamp, title, size_str)
    let mut entries: Vec<(String, String, String)> = Vec::new();
    let mut paths = Vec::new();
    for (path, mtime, size) in files.iter() {
        let fname = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ts = mtime.as_ref().map(|t| DateTime::<Local>::from(*t).format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_else(|| "unknown".into());

        // derive title from filename format: [title]_[id].json
        let mut base = fname.clone();
        if base.ends_with(".json") {
            base.truncate(base.len() - 5);
        }
        let title = if let Some(idx) = base.rfind('_') {
            let t = &base[..idx];
            if t.is_empty() { "".to_string() } else { t.to_string() }
        } else {
            "".to_string()
        };

        let size_str = human_size(*size);
        entries.push((ts, title, size_str));
        paths.push(path.clone());
    }

    // initial preview for selected item (used by TTY and non-TTY flows)
    let mut preview = String::new();
    if let Some(p) = paths.get(0) {
        preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
    }
    if entries.is_empty() {
        preview = "(no preview available)".to_string();
    }

    // filesystem watcher: notify main thread when dumps_dir content changes
    // Use a typed WatchEvent channel so we can send granular events to the UI.
    let (fs_tx, fs_rx) = std::sync::mpsc::channel::<WatchEvent>();
    let watch_dir = dumps_dir.clone();

    // Start a notify-based watcher. Translate notify events into WatchEvent values and send them
    // to the TUI thread. If notify fails at runtime, spawn a polling fallback.
    {
        let tx = fs_tx.clone();
        let watch_dir2 = watch_dir.clone();
        thread::spawn(move || {
            use notify::{Watcher, RecursiveMode, RecommendedWatcher, EventKind};
            use std::sync::mpsc::RecvTimeoutError;

            if let Err(e) = (|| -> Result<(), Box<dyn std::error::Error>> {
                let (local_tx, rx) = std::sync::mpsc::channel();
                let mut watcher: RecommendedWatcher = RecommendedWatcher::new(local_tx, notify::Config::default())?;
                watcher.watch(&watch_dir2, RecursiveMode::NonRecursive)?;

                loop {
                    match rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(Ok(ev)) => {
                            for p in ev.paths.iter() {
                                let we = match &ev.kind {
                                    EventKind::Create(_) => WatchEvent::Created(p.clone()),
                                    EventKind::Modify(_) => WatchEvent::Modified(p.clone()),
                                    EventKind::Remove(_) => WatchEvent::Removed(p.clone()),
                                    _ => WatchEvent::Rescan,
                                };
                                let _ = tx.send(we);
                            }
                        }
                        Ok(Err(e)) => {
                            eprintln!("notify event error: {}", e);
                            let _ = tx.send(WatchEvent::Rescan);
                        }
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(_) => break,
                    }
                }

                Ok(())
            })() {
                eprintln!("notify-based watcher failed: {} - falling back to polling", e);

                // Fallback polling implementation in case notify fails at runtime
                let tx2 = tx.clone();
                let watch_dir3 = watch_dir2.clone();
                std::thread::spawn(move || {
                    let mut last: HashMap<String, std::time::SystemTime> = HashMap::new();
                    loop {
                        let mut current: HashMap<String, std::time::SystemTime> = HashMap::new();
                        if let Ok(rd) = std::fs::read_dir(&watch_dir3) {
                            for e in rd.filter_map(|e| e.ok()) {
                                let p = e.path();
                                if p.is_file() {
                                    if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                                        current.insert(p.to_string_lossy().to_string(), m);
                                    }
                                }
                            }
                        }
                        if current != last {
                            for (k, m) in current.iter() {
                                if !last.contains_key(k) {
                                    let _ = tx2.send(WatchEvent::Created(std::path::PathBuf::from(k)));
                                } else if last.get(k).map(|t| t != m).unwrap_or(false) {
                                    let _ = tx2.send(WatchEvent::Modified(std::path::PathBuf::from(k)));
                                }
                            }
                            for k in last.keys() {
                                if !current.contains_key(k) {
                                    let _ = tx2.send(WatchEvent::Removed(std::path::PathBuf::from(k)));
                                }
                            }
                            last = current;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                    }
                });
            }
        });
    }

    // If stdout is not a TTY, fall back to a simple non-interactive listing + preview
    if !atty::is(Stream::Stdout) {
        println!("Dumps (from {}):", dumps_dir.display());
        if entries.is_empty() {
            println!("(no dumps found)");
        } else {
            for (i, (ts, title, size_str)) in entries.iter().enumerate() {
                let display_title = if title.len() > 40 { format!("{}...", &title[..37]) } else { title.clone() };
                if display_title.is_empty() {
                    // omit the title column when it's empty to avoid extra whitespace
                    println!("{}: {:<19} {:>8}", i + 1, ts, size_str);
                } else {
                    println!("{}: {:<19}  {:<40} {:>8}", i + 1, ts, display_title, size_str);
                }
            }
        }
        println!("\n--- Preview (first item) ---\n");
        if preview.is_empty() {
            println!("(no preview available)");
        } else {
            println!("{}", preview);
        }
        return Ok(());
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // interactive list state
    let mut state = TableState::default();
    if entries.is_empty() {
        state.select(None);
    } else {
        state.select(Some(0));
    }

    // Setup signal handling to gracefully exit and restore terminal
    let (sig_tx, sig_rx) = channel();
    let mut signals = Signals::new(&[SIGINT, SIGTERM, SIGQUIT]).unwrap();
    thread::spawn(move || {
        for sig in signals.forever() {
            // send the signal number to the main thread
            let _ = sig_tx.send(sig);
        }
    });

    // render loop
    loop {
        // handle filesystem events (non-blocking) and apply incremental updates
        if let Ok(ev) = fs_rx.try_recv() {
            apply_watch_event(ev, &dumps_dir, &mut entries, &mut paths, &mut state, &mut preview);
        }
        terminal.draw(|f| {
            let size = f.size();
            // reserve one line at the bottom for a full-width status line
            let vchunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(size.height.saturating_sub(1)), Constraint::Length(1)].as_ref())
                .split(size);
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
                .split(vchunks[0]);

            // Render a table with two columns: timestamp | title
            let rows: Vec<Row> = entries
                .iter()
                .map(|(ts, title, _size_str)| {
                    let title_trunc = if title.len() > 60 { title.chars().take(57).collect::<String>() + "..." } else { title.clone() };
                    Row::new(vec![Cell::from(ts.clone()), Cell::from(title_trunc)])
                })
                .collect();

            let table = Table::new(rows)
                .block(Block::default().borders(Borders::ALL).title("Dumps"))
                .widths(&[Constraint::Length(19), Constraint::Min(10)])
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

            if entries.is_empty() {
                // draw an empty placeholder in the table area
                let empty = Paragraph::new("(no dumps found)").block(Block::default().borders(Borders::ALL).title("Dumps"));
                f.render_widget(empty, chunks[0]);
            } else {
                f.render_stateful_widget(table, chunks[0], &mut state);
            }

            // Use RawPreview to render lines truncated to the available width with no wrapping.
            let preview_widget = RawPreview { text: &preview };
            let block = Block::default().borders(Borders::ALL).title("Preview");
            let inner = block.inner(chunks[1]);
            f.render_widget(block, chunks[1]);
            f.render_widget(preview_widget, inner);

            // status line spanning full width, below the boxes
            // build status line: left = full title, right = size (right-aligned)
            let status = if let Some((_ts, title, size_str)) = state.selected().and_then(|i| entries.get(i)) {
                let width = vchunks[1].width as usize;
                let size_w = UnicodeWidthStr::width(size_str.as_str());
                if size_w >= width {
                    // size itself doesn't fit; truncate it to the available width
                    let mut acc = String::new();
                    let mut cur_w = 0usize;
                    for ch in size_str.chars() {
                        let cw = UnicodeWidthStr::width(ch.to_string().as_str());
                        if cur_w + cw > width { break; }
                        acc.push(ch);
                        cur_w += cw;
                    }
                    Paragraph::new(acc)
                } else {
                    let title_w = UnicodeWidthStr::width(title.as_str());
                    let max_title_w = width.saturating_sub(size_w + 1);
                    let title_display = if title_w > max_title_w {
                        // truncate and add ellipsis
                        let mut acc = String::new();
                        let mut cur_w = 0usize;
                        for ch in title.chars() {
                            let cw = UnicodeWidthStr::width(ch.to_string().as_str());
                            if cur_w + cw + 3 > max_title_w { break; }
                            acc.push(ch);
                            cur_w += cw;
                        }
                        acc.push_str("...");
                        acc
                    } else {
                        title.clone()
                    };
                    let pad_count = width.saturating_sub(UnicodeWidthStr::width(title_display.as_str()) + size_w);
                    let pad = std::iter::repeat('\u{00A0}').take(pad_count).collect::<String>();
                    Paragraph::new(format!("{}{}{}", title_display, pad, size_str))
                }
            } else {
                Paragraph::new("")
            };
            f.render_widget(status, vchunks[1]);
        })?;

        // handle input or signals
        if event::poll(Duration::from_millis(200))? {
            if let CEvent::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Esc => break,
                    KeyCode::Up => {
                        if let Some(i) = state.selected() {
                            let len = entries.len();
                            let ni = if i == 0 { len - 1 } else { i - 1 };
                            state.select(Some(ni));
                            if let Some(p) = paths.get(ni) {
                                preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                            }
                        }
                    }
                    KeyCode::Down => {
                        if let Some(i) = state.selected() {
                            let ni = (i + 1) % entries.len();
                            state.select(Some(ni));
                            if let Some(p) = paths.get(ni) {
                                preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                            }
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(i) = state.selected() {
                            // open pager while keeping this process running; leave alternate screen,
                            // run pager, then re-enter alternate screen and continue
                            let path = paths[i].clone();

                            // detect pager
                            let pager = if Command::new("jless").arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false) {
                                "jless"
                            } else if Command::new("less").arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false) {
                                "less"
                            } else {
                                // fallback to cat
                                "cat"
                            };

                            // restore terminal to normal
                            let _ = disable_raw_mode();
                            let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
                            let _ = terminal.show_cursor();

                            // run pager (blocking) but keep parent able to receive signals and forward them
                            let child = if pager == "cat" {
                                Command::new("cat").arg(&path).spawn()
                            } else {
                                // spawn pager normally so it keeps the controlling terminal and is interactive
                                let mut c = Command::new(pager);
                                c.arg(&path);
                                c.spawn()
                            };

                            if let Ok(mut child) = child {
                                // while child is running, forward any received signals to the child
                                loop {
                                    // poll for signals
                                    if let Ok(sig) = sig_rx.try_recv() {
                                        // translate signal number to libc signal and forward
                                        unsafe {
                                            libc::kill(child.id() as i32, sig);
                                        }
                                    }
                                    match child.try_wait() {
                                        Ok(Some(_)) => break,
                                        Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                                        Err(_) => break,
                                    }
                                }
                            }

                            // Recreate terminal backend and re-enter alternate screen so the TUI is fully restored.
                            // This is more robust across pagers/terminals than re-using the old backend.
                            let mut stdout = io::stdout();
                            execute!(stdout, EnterAlternateScreen)?;
                            let backend = CrosstermBackend::new(stdout);
                            // replace the terminal with a freshly initialized one
                            terminal = Terminal::new(backend)?;
                            let _ = enable_raw_mode();

                            // refresh preview for current selection
                            preview = read_preview(&path).unwrap_or_else(|e| format!("failed to read preview: {}", e));

                            // Small delay to allow terminal to settle, then redraw the TUI so the screen is restored
                            std::thread::sleep(Duration::from_millis(100));
                            let _ = terminal.draw(|f| {
                            let size = f.size();
                            let vchunks = Layout::default()
                                .direction(Direction::Vertical)
                                .constraints([Constraint::Length(size.height.saturating_sub(1)), Constraint::Length(1)].as_ref())
                                .split(size);
                            let chunks = Layout::default()
                                .direction(Direction::Horizontal)
                                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
                                .split(vchunks[0]);

                            // rebuild and render the Table for redraw (timestamp | title)
                            let rows: Vec<Row> = entries
                                .iter()
                                .map(|(ts, title, _size_str)| {
                                    let title_trunc = if title.len() > 60 { title.chars().take(57).collect::<String>() + "..." } else { title.clone() };
                                    Row::new(vec![Cell::from(ts.clone()), Cell::from(title_trunc)])
                                })
                                .collect();

                            let table = Table::new(rows)
                                .block(Block::default().borders(Borders::ALL).title("Dumps"))
                                .widths(&[Constraint::Length(19), Constraint::Min(10)]);

                            f.render_stateful_widget(table, chunks[0], &mut state);

                                let preview_widget = RawPreview { text: &preview };
                                let block = Block::default().borders(Borders::ALL).title("Preview");
                                let inner = block.inner(chunks[1]);
                                f.render_widget(block, chunks[1]);
                                f.render_widget(preview_widget, inner);

                                // status line spanning full width, below the boxes
                                let status = if let Some((_ts, title, size_str)) = state.selected().and_then(|i| entries.get(i)) {
                                    let width = vchunks[1].width as usize;
                                    let size_w = UnicodeWidthStr::width(size_str.as_str());
                                    if size_w >= width {
                                        let mut acc = String::new();
                                        let mut cur_w = 0usize;
                                        for ch in size_str.chars() {
                                            let cw = UnicodeWidthStr::width(ch.to_string().as_str());
                                            if cur_w + cw > width { break; }
                                            acc.push(ch);
                                            cur_w += cw;
                                        }
                                        Paragraph::new(acc)
                                    } else {
                                        let title_w = UnicodeWidthStr::width(title.as_str());
                                        let max_title_w = width.saturating_sub(size_w + 1);
                                        let title_display = if title_w > max_title_w {
                                            let mut acc = String::new();
                                            let mut cur_w = 0usize;
                                            for ch in title.chars() {
                                                let cw = UnicodeWidthStr::width(ch.to_string().as_str());
                                                if cur_w + cw + 3 > max_title_w { break; }
                                                acc.push(ch);
                                                cur_w += cw;
                                            }
                                            acc.push_str("...");
                                            acc
                                        } else {
                                            title.clone()
                                        };
                                        let pad_count = width.saturating_sub(UnicodeWidthStr::width(title_display.as_str()) + size_w);
                                        let pad = std::iter::repeat('\u{00A0}').take(pad_count).collect::<String>();
                                        Paragraph::new(format!("{}{}{}", title_display, pad, size_str))
                                    }
                                } else {
                                    Paragraph::new("")
                                };
                                f.render_widget(status, vchunks[1]);
                            });
                        }
                    }
                    _ => {}
                }
            }
        }

        // check for signals
        if let Ok(sig) = sig_rx.try_recv() {
            // on signal, break loop and restore terminal
            eprintln!("received signal {} - exiting", sig);
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // Ensure we stop the server child we spawned so the server is not independent of the TUI.
    if let Some(mut child) = server_child {
        // try a graceful shutdown
        let _ = child.kill();
        let _ = child.wait();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn test_human_size_basic() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(1024 * 1024), "1.0 MiB");
        assert_eq!(human_size(1024_u64.pow(3)), "1.0 GiB");
    }

    #[test]
    fn test_build_entry_from_path_extracts_title_and_size() {
        // create a temporary file in the system temp dir
        let mut path = std::env::temp_dir();
        let fname = format!("2026-03-18_test-title-{}_.json", uuid::Uuid::new_v4());
        path.push(fname);

        let data = b"{\"x\":1}\n";
        {
            let mut f = fs::File::create(&path).expect("create temp file");
            f.write_all(data).expect("write data");
        }

        // ensure file metadata is available
        std::thread::sleep(std::time::Duration::from_millis(10));

        let res = build_entry_from_path(&path).expect("should build entry");
        let ((ts, title, size_str), _mtime) = res;

        // title should match (stripping .json and underscore logic)
        assert!(title.contains("test-title"));
        // size_str should reflect the bytes we wrote (small -> bytes)
        assert!(size_str.ends_with("B") || size_str.contains("KiB"));
        // ts should be a non-empty timestamp string
        assert!(!ts.is_empty());

        // cleanup
        let _ = fs::remove_file(&path);
    }
}
