use std::io::{self};
use std::{fs, process::Command, time::SystemTime, time::Duration};
use chrono::{DateTime, Local};
use crossterm::{execute, terminal::{enable_raw_mode, disable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}, event};
use crossterm::event::{Event as CEvent, KeyCode};
use tui::backend::CrosstermBackend;
use tui::Terminal;
use tui::widgets::{Block, Borders, List, ListItem};
use tui::layout::{Layout, Constraint, Direction};
use tui::widgets::ListState;

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

    // render loop
    let mut selected_path: Option<std::path::PathBuf> = None;
    loop {
        terminal.draw(|f| {
            let size = f.size();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100)].as_ref())
                .split(size);

            let mut list = List::new(items.clone()).block(Block::default().borders(Borders::ALL).title("Dumps"));
            list = list.highlight_symbol("» ");
            f.render_stateful_widget(list, chunks[0], &mut state);
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
                        }
                    }
                    KeyCode::Down => {
                        if let Some(i) = state.selected() {
                            let ni = (i + 1) % items.len();
                            state.select(Some(ni));
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(i) = state.selected() {
                            selected_path = Some(paths[i].clone());
                        }
                        break;
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
