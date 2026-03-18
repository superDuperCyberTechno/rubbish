use dialoguer::{theme::ColorfulTheme, Select};
use std::{fs, process::Command, path::Path};

fn main() {
    let dumps = match fs::read_dir("dumps") {
        Ok(rd) => rd.filter_map(|e| e.ok()).filter(|e| e.path().is_file()).collect::<Vec<_>>(),
        Err(_) => {
            println!("no dumps directory");
            return;
        }
    };

    if dumps.is_empty() {
        println!("no dumps found");
        return;
    }

    let items: Vec<String> = dumps.iter().map(|e| {
        let fname = e.file_name().to_string_lossy().into_owned();
        fname
    }).collect();

    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select a dump to open with jless")
        .items(&items)
        .default(0)
        .interact();

    match selection {
        Ok(idx) => {
            let path = Path::new("dumps").join(&items[idx]);
            // spawn jless
            let _ = Command::new("jless").arg(path).status();
        }
        Err(e) => eprintln!("selection failed: {}", e),
    }
}
