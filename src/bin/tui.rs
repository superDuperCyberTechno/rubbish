use dialoguer::{theme::ColorfulTheme, Select};
use std::{fs, process::Command, path::Path};
use chrono::{DateTime, Local};

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

fn main() {
    let entries = match fs::read_dir("dumps") {
        Ok(rd) => rd.filter_map(|e| e.ok()).filter(|e| e.path().is_file()).collect::<Vec<_>>(),
        Err(_) => {
            println!("no dumps directory");
            return;
        }
    };

    if entries.is_empty() {
        println!("no dumps found");
        return;
    }

    // Build display items with filename, modified timestamp and size
    let mut items: Vec<String> = Vec::with_capacity(entries.len());
    let mut paths: Vec<std::path::PathBuf> = Vec::with_capacity(entries.len());

    for e in entries.iter() {
        let path = e.path();
        let fname = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let meta = path.metadata();
        let (ts, size) = if let Ok(m) = meta {
            let size = m.len();
            let ts = m.modified().ok().and_then(|t| {
                DateTime::<Local>::from(t).into();
                // Convert SystemTime to DateTime<Local>
                Some(DateTime::<Local>::from(t))
            });
            let ts_str = ts.map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_else(|| "unknown".into());
            (ts_str, human_size(size))
        } else {
            ("unknown".into(), "?".into())
        };

        items.push(format!("{}  —  {}  ({})", fname, ts, size));
        paths.push(path);
    }

    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select a dump to open with jless")
        .items(&items)
        .default(0)
        .interact();

    match selection {
        Ok(idx) => {
            let path = &paths[idx];
            let _ = Command::new("jless").arg(path).status();
        }
        Err(e) => eprintln!("selection failed: {}", e),
    }
}
