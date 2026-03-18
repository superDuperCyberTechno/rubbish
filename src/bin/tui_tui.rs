use std::io;
use std::io::Read;
use std::{fs, process::{Command, Stdio}, time::{SystemTime, Duration}, env};
use std::net::{SocketAddr, TcpStream};
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
use tui::widgets::{Block, Borders, Paragraph, Wrap, Table, Row, Cell};
use tui::style::{Style, Modifier};
use tui::layout::{Layout, Constraint, Direction};
use tui::widgets::TableState;

fn read_preview(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    // Read up to 64KB for preview and pretty-print JSON if possible
    let f = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(f);
    let mut buf = String::new();
    reader.take(64 * 1024).read_to_string(&mut buf)?;

    // Try to pretty-print JSON
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&buf) {
        return Ok(serde_json::to_string_pretty(&json)?);
    }

    // Fallback: return raw (trimmed)
    Ok(buf)
}

fn human_size(bytes: u64) -> String {
    // Use IEC (binary) units: KiB, MiB, GiB (base 1024)
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KIB {
        format!("{} B", bytes)
    } else if b < KIB * KIB {
        format!("{:.1} KiB", b / KIB)
    } else if b < KIB * KIB * KIB {
        format!("{:.1} MiB", b / (KIB * KIB))
    } else {
        format!("{:.1} GiB", b / (KIB * KIB * KIB))
    }
}

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

        let title = if let Some(idx) = fname.find('_') {
            let mut t = fname[idx + 1..].to_string();
            if t.ends_with(".json") {
                t.truncate(t.len() - 5);
            }
            if t.is_empty() { "(no title)".to_string() } else { t }
        } else {
            "(no title)".to_string()
        };

        let size_str = human_size(*size);
        entries.push((ts, title, size_str));
        paths.push(path.clone());
    }

    (entries, paths)
}

#[derive(Debug)]
enum WatchEvent {
    Created(()),
    Modified(()),
    Removed(()),
    Rescan,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // start the server if it's not already running on localhost:7771
    let server_addr: SocketAddr = "127.0.0.1:7771".parse().unwrap();
    let server_up = TcpStream::connect_timeout(&server_addr, Duration::from_millis(200)).is_ok();
    if !server_up {
        // try to use compiled binary if present, otherwise fall back to `cargo run --bin rubbish`
        let server_spawn = if std::path::Path::new("target/debug/rubbish").exists() {
            Command::new("./target/debug/rubbish").stdout(Stdio::null()).stderr(Stdio::null()).spawn()
        } else {
            Command::new("cargo").args(["run","--bin","rubbish"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
        };

        match server_spawn {
            Ok(child) => {
                let pid = child.id();
                eprintln!("started rubbish server (pid={})", pid);
                // try to record pid for later management; ignore errors
                let _ = std::fs::write("server.pid", format!("{}\n", pid));
            }
            Err(e) => {
                eprintln!("failed to start rubbish server: {}", e);
            }
        }
        // give server a moment to bind
        std::thread::sleep(Duration::from_millis(300));
    }

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

        // derive title from filename: <timestamp>_<title>.json or empty
        let title = if let Some(idx) = fname.find('_') {
            let mut t = fname[idx + 1..].to_string();
            if t.ends_with(".json") {
                t.truncate(t.len() - 5);
            }
            if t.is_empty() { "(no title)".to_string() } else { t }
        } else {
            "(no title)".to_string()
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
                            for _p in ev.paths.iter() {
                                let we = match &ev.kind {
                                    EventKind::Create(_) => WatchEvent::Created(()),
                                    EventKind::Modify(_) => WatchEvent::Modified(()),
                                    EventKind::Remove(_) => WatchEvent::Removed(()),
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
                                    let _ = tx2.send(WatchEvent::Created(()));
                                } else if last.get(k).map(|t| t != m).unwrap_or(false) {
                                    let _ = tx2.send(WatchEvent::Modified(()));
                                }
                            }
                            for k in last.keys() {
                                if !current.contains_key(k) {
                                    let _ = tx2.send(WatchEvent::Removed(()));
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
        // handle filesystem events (non-blocking) and refresh entries/paths/preview
        if let Ok(ev) = fs_rx.try_recv() {
                match ev {
                WatchEvent::Created(_) | WatchEvent::Modified(_) | WatchEvent::Removed(_) | WatchEvent::Rescan => {
                    let (new_entries, new_paths) = scan_dumps(&dumps_dir);
                    entries = new_entries;
                    paths = new_paths;
                    if entries.is_empty() {
                        preview = "(no preview available)".to_string();
                        state.select(None);
                    } else {
                        // ensure selection is valid
                        match state.selected() {
                            Some(i) if i < entries.len() => {
                                if let Some(p) = paths.get(i) {
                                    preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                                }
                            }
                            _ => {
                                state.select(Some(0));
                                if let Some(p) = paths.get(0) {
                                    preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                                }
                            }
                        }
                    }
                }
            }
        }
        terminal.draw(|f| {
            let size = f.size();
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
                .split(size);

            // Render a table with three columns: timestamp | title | size
            let rows: Vec<Row> = entries
                .iter()
                .map(|(ts, title, size_str)| {
                    let title_trunc = if title.len() > 60 { title.chars().take(57).collect::<String>() + "..." } else { title.clone() };
                    Row::new(vec![Cell::from(ts.clone()), Cell::from(title_trunc), Cell::from(size_str.clone())])
                })
                .collect();

            let table = Table::new(rows)
                .block(Block::default().borders(Borders::ALL).title("Dumps"))
                .widths(&[Constraint::Length(19), Constraint::Percentage(70), Constraint::Length(12)])
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

            if entries.is_empty() {
                // draw an empty placeholder in the table area
                let empty = Paragraph::new("(no dumps found)").block(Block::default().borders(Borders::ALL).title("Dumps"));
                f.render_widget(empty, chunks[0]);
            } else {
                f.render_stateful_widget(table, chunks[0], &mut state);
            }

            let paragraph = Paragraph::new(preview.clone())
                .block(Block::default().borders(Borders::ALL).title("Preview"))
                .wrap(Wrap { trim: true });
            f.render_widget(paragraph, chunks[1]);
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
                                let chunks = Layout::default()
                                    .direction(Direction::Horizontal)
                                    .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
                                    .split(size);

                            // rebuild and render the Table for redraw
                            let rows: Vec<Row> = entries
                                .iter()
                                .map(|(ts, title, size_str)| {
                                    let title_trunc = if title.len() > 60 { title.chars().take(57).collect::<String>() + "..." } else { title.clone() };
                                    Row::new(vec![Cell::from(ts.clone()), Cell::from(title_trunc), Cell::from(size_str.clone())])
                                })
                                .collect();

                            let table = Table::new(rows)
                                .block(Block::default().borders(Borders::ALL).title("Dumps"))
                                .widths(&[Constraint::Length(19), Constraint::Percentage(70), Constraint::Length(10)]);

                            f.render_stateful_widget(table, chunks[0], &mut state);

                                let paragraph = Paragraph::new(preview.clone())
                                    .block(Block::default().borders(Borders::ALL).title("Preview"))
                                    .wrap(Wrap { trim: true });
                                f.render_widget(paragraph, chunks[1]);
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

    Ok(())
}
