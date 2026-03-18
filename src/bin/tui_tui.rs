use std::io::{self, Read};
use std::{fs, process::Command, time::SystemTime, time::Duration};
use chrono::{DateTime, Local};
use crossterm::{execute, terminal::{enable_raw_mode, disable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}, event};
use crossterm::event::{Event as CEvent, KeyCode};
use tui::backend::CrosstermBackend;
use tui::Terminal;
use tui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use tui::layout::{Layout, Constraint, Direction};
use tui::widgets::ListState;

fn read_preview(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    // Read up to 64KB for preview and pretty-print JSON if possible
    let f = std::fs::File::open(path)?;
    let mut buf = String::new();
    let _ = std::io::Read::by_ref(&mut &f).take(64 * 1024).read_to_string(&mut buf);

    // Try to pretty-print JSON
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&buf) {
        let pretty = serde_json::to_string_pretty(&json)?;
        return Ok(pretty);
    }

    // Fallback: return raw (trimmed)
    Ok(buf)
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{} B", bytes)
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else {
        format!("{:.1} GB", b / (KB * KB * KB))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // collect files with metadata and sort by modified time (newest first)
    let rd = fs::read_dir("dumps")?;
    let mut files: Vec<(std::path::PathBuf, Option<SystemTime>, u64)> = rd
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

    if files.is_empty() {
        println!("no dumps found");
        return Ok(());
    }

    files.sort_by_key(|(_, mtime, _)| mtime.unwrap_or(SystemTime::UNIX_EPOCH));
    files.reverse();

    let mut items = Vec::new();
    let mut paths = Vec::new();
    for (path, mtime, size) in files.iter() {
        let fname = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ts = mtime.as_ref().map(|t| DateTime::<Local>::from(*t).format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_else(|| "unknown".into());
        items.push(ListItem::new(format!("{}  —  {}  ({})", fname, ts, human_size(*size))));
        paths.push(path.clone());
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // interactive list state
    let mut state = ListState::default();
    state.select(Some(0));

    // initial preview for selected item
    let mut preview = String::new();
    if let Some(p) = paths.get(0) {
        preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
    }

    // render loop
    let mut selected_path: Option<std::path::PathBuf> = None;
    loop {
        terminal.draw(|f| {
            let size = f.size();
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
                .split(size);

            let mut list = List::new(items.clone()).block(Block::default().borders(Borders::ALL).title("Dumps"));
            list = list.highlight_symbol("» ");
            f.render_stateful_widget(list, chunks[0], &mut state);

            let paragraph = Paragraph::new(preview.clone())
                .block(Block::default().borders(Borders::ALL).title("Preview"))
                .wrap(Wrap { trim: true });
            f.render_widget(paragraph, chunks[1]);
        })?;

        // handle input
        if event::poll(Duration::from_millis(200))? {
            if let CEvent::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Esc => break,
                    KeyCode::Up => {
                        if let Some(i) = state.selected() {
                            let len = items.len();
                            let ni = if i == 0 { len - 1 } else { i - 1 };
                            state.select(Some(ni));
                            if let Some(p) = paths.get(ni) {
                                preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                            }
                        }
                    }
                    KeyCode::Down => {
                        if let Some(i) = state.selected() {
                            let ni = (i + 1) % items.len();
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

                            // run pager (blocking)
                            if pager == "cat" {
                                let _ = Command::new("cat").arg(&path).status();
                            } else {
                                let _ = Command::new(pager).arg(&path).status();
                            }

                            // re-enter alternate screen and resume
                            let _ = execute!(terminal.backend_mut(), EnterAlternateScreen);
                            let _ = enable_raw_mode();

                            // refresh preview for current selection
                            preview = read_preview(&path).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // If a selection was made, open with pager fallback
    if let Some(path) = selected_path {
        let pager = if Command::new("jless").arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false) {
            "jless"
        } else if Command::new("less").arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false) {
            "less"
        } else {
            println!("neither jless nor less found in PATH; printing file contents below:\n");
            let _ = Command::new("cat").arg(path).status();
            return Ok(());
        };

        if let Err(e) = Command::new(pager).arg(path).status() {
            eprintln!("failed to spawn {}: {}", pager, e);
        }
    }

    Ok(())
}
