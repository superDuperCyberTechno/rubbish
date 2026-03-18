use dialoguer::{theme::ColorfulTheme, Select};
use std::{fs, process::{Command, Stdio}, time::SystemTime};
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
    let rd = match fs::read_dir("dumps") {
        Ok(rd) => rd,
        Err(_) => {
            println!("no dumps directory");
            return;
        }
    };

    // Collect entries (only files) and their metadata, then sort by modified time desc
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
        .collect::<Vec<_>>();

    if files.is_empty() {
        println!("no dumps found");
        return;
    }

    files.sort_by_key(|(_, mtime, _)| {
        // sort descending: map to SystemTime, missing -> UNIX_EPOCH
        mtime.unwrap_or(SystemTime::UNIX_EPOCH)
    });
    files.reverse();

    // Build display items with filename, modified timestamp and size
    let mut items: Vec<String> = Vec::with_capacity(files.len());
    let mut paths: Vec<std::path::PathBuf> = Vec::with_capacity(files.len());

    for (path, mtime, size) in files.iter() {
        let fname = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ts_str = mtime
            .as_ref()
            .map(|t| DateTime::<Local>::from(*t).format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "unknown".into());

        // derive a human title from filename: <ts>_<title>.json or just <ts>.json
        let title = if let Some(idx) = fname.find('_') {
            let mut t = fname[idx + 1..].to_string();
            if t.ends_with(".json") {
                t.truncate(t.len() - 5);
            }
            t
        } else {
            String::new()
        };

        // show timestamp+size on first row, title (from rubbish-title) on second row
        let title_display = if title.is_empty() { "(no title)".to_string() } else { title.clone() };
        items.push(format!("{} ({})\n{}", ts_str, human_size(*size), title_display));
        paths.push(path.clone());
    }

    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select a dump to open with jless")
        .items(&items)
        .default(0)
        .interact();

    match selection {
        Ok(idx) => {
            let path = &paths[idx];
            // prefer jless if available, fallback to less, then cat
            let pager = if Command::new("jless").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false) {
                "jless"
            } else if Command::new("less").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false) {
                "less"
            } else {
                // last resort: print a helpful message and show with cat
                println!("neither jless nor less found in PATH; printing file contents below:\n");
                let _ = Command::new("cat").arg(path).status();
                return;
            };

            if let Err(e) = Command::new(pager).arg(path).status() {
                eprintln!("failed to spawn {}: {}", pager, e);
            }
        }
        Err(e) => eprintln!("selection failed: {}", e),
    }
}
