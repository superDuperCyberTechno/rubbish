use std::io::{self, Read};
use std::{fs, process::Command};
use chrono::{DateTime, Local};
use crossterm::{execute, terminal::{enable_raw_mode, disable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}};
use tui::backend::CrosstermBackend;
use tui::Terminal;
use tui::widgets::{Block, Borders, List, ListItem};
use tui::layout::{Layout, Constraint, Direction};

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
    let entries = fs::read_dir("dumps")?.filter_map(|e| e.ok()).filter(|e| e.path().is_file()).collect::<Vec<_>>();
    if entries.is_empty() {
        println!("no dumps found");
        return Ok(());
    }

    let mut items = Vec::new();
    let mut paths = Vec::new();
    for e in entries.iter() {
        let path = e.path();
        let fname = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let meta = path.metadata()?;
        let size = meta.len();
        let ts = meta.modified().ok().map(|t| DateTime::<Local>::from(t)).map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_else(|| "unknown".into());
        items.push(ListItem::new(format!("{}  —  {}  ({})", fname, ts, human_size(size))));
        paths.push(path);
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|f| {
        let size = f.size();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(size);

        let list = List::new(items.clone()).block(Block::default().borders(Borders::ALL).title("Dumps"));
        f.render_widget(list, chunks[0]);
    })?;

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // After showing list, just spawn jless on the first item as a simple flow for now
    if let Some(path) = paths.get(0) {
        let _ = Command::new("jless").arg(path).status();
    }

    Ok(())
}
