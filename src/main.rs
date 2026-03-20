use axum::{http::HeaderMap, response::IntoResponse, routing::post, Router};
// run with axum's `serve` helper using a TcpListener
use tokio::net::TcpListener;
use chrono::Utc;
use std::{fs, io::Write, net::SocketAddr};
use std::path::PathBuf;
use std::env;
use tokio::signal;
use tracing::{error, info};
use tracing_appender::rolling;

// Embed the TUI module (kept in `src/tui.rs`) and expose its runner
mod tui;
pub use tui::run_tui as run_tui;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure tracing to append logs to ~/.local/share/rubbish/info.log
    let log_path = match env::var("XDG_DATA_HOME") {
        Ok(x) if !x.is_empty() => PathBuf::from(x).join("rubbish").join("info.log"),
        _ => match env::var("HOME") {
            Ok(h) => PathBuf::from(h).join(".local").join("share").join("rubbish").join("info.log"),
            Err(_) => PathBuf::from("./info.log"),
        },
    };
    if let Some(parent) = log_path.parent() { let _ = fs::create_dir_all(parent); }
    // rolling::never will append to the same file; build owned args to avoid borrowing temporaries
    let parent_dir = log_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let file_name = log_path.file_name().and_then(|s| s.to_str()).unwrap_or("info.log").to_string();
    let file_appender = rolling::never(parent_dir, file_name);
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt().with_writer(non_blocking).init();

    // Start the TUI in a background thread so we can run the Axum server in this Tokio runtime.
    // Use a oneshot channel so the TUI can signal the server to shut down when it exits.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let tui_handle = std::thread::spawn(move || {
        if let Err(e) = crate::tui::run_tui() {
            eprintln!("TUI error: {}", e);
        }
        // notify main to shut down the server when the TUI exits (ignore send errors)
        let _ = shutdown_tx.send(());
    });

    // Run HTTP server using Axum; bind to localhost:7771 as before.
    let app = Router::new().route("/dump", post(handle_dump));
    let addr = SocketAddr::from(([127, 0, 0, 1], 7771));
    info!(%addr, "starting rubbish dump server");

    let listener = TcpListener::bind(addr).await.expect("failed to bind");
    let server = axum::serve(listener, app);

    // Shutdown either on Ctrl-C or when the TUI signals via the oneshot channel.
    let graceful = server.with_graceful_shutdown(async {
        tokio::select! {
            _ = signal::ctrl_c() => {
                info!("shutting down");
            }
            _ = shutdown_rx => {
                info!("tui exited; shutting down server");
            }
        }
    });

    if let Err(e) = graceful.await {
        error!(%e, "server error");
    }

    // Wait for the TUI thread to finish before exiting
    let _ = tui_handle.join();
    Ok(())
}

async fn handle_dump(headers: HeaderMap, body: axum::body::Bytes) -> impl IntoResponse {
    // determine dumps directory (XDG_DATA_HOME/rubbish/dumps or ~/.local/share/rubbish/dumps)
    let dumps_dir = match env::var("XDG_DATA_HOME") {
        Ok(x) if !x.is_empty() => PathBuf::from(x).join("rubbish").join("dumps"),
        _ => match env::var("HOME") {
            Ok(h) => PathBuf::from(h).join(".local").join("share").join("rubbish").join("dumps"),
            Err(_) => PathBuf::from("./dumps"),
        },
    };

    // Ensure dumps directory exists
    if let Err(e) = fs::create_dir_all(&dumps_dir) {
        error!(%e, "failed to create dumps dir");
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "failed to create dumps dir");
    }

    // build filename as: [ulid].json (dump name is ULID)
    let id = ulid::Ulid::new().to_string();
    let path = dumps_dir.join(format!("{}.json", id));

    match save_bytes(&path, &body).await {
        Ok(_) => {
            // Build metadata object with optional title and tags and write atomically next to the dump
            let title_str = headers
                .get("rubbish-title")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();

            let mut tags_vec: Vec<String> = Vec::new();
            if let Some(tags_val) = headers.get("rubbish-tags") {
                if let Ok(tags_str) = tags_val.to_str() {
                    for t in tags_str.split(',') {
                        let tt = t.trim();
                        if !tt.is_empty() {
                            tags_vec.push(tt.to_string());
                        }
                    }
                }
            }

            let meta = serde_json::json!({
                "title": title_str,
                "tags": tags_vec,
                // unix timestamp in seconds added by the server
                "timestamp": Utc::now().timestamp(),
            });
            let meta_path = path.with_extension("metadata.json");
            if let Err(e) = write_text_atomic(&meta_path, &serde_json::to_string_pretty(&meta).unwrap_or_else(|_| "{}".to_string())) {
                error!(%e, file = %meta_path.display(), "failed to write metadata file");
            }
            info!(file = %path.display(), "saved dump");
            (axum::http::StatusCode::OK, "ok")
        }
        Err(e) => {
            error!(%e, "failed to write dump");
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "write failed")
        }
    }
}

// legacy helpers removed: filenames are ULID-only and title sanitization is unused

async fn save_bytes(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    // write atomically: write to tmp then rename
    let tmp = path.with_extension("json.tmp");
    // Try to pretty-print JSON before saving. If parsing fails, fall back to original bytes.
    let to_write: Vec<u8> = match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(v) => match serde_json::to_string_pretty(&v) {
            Ok(mut s) => {
                // ensure trailing newline for readability
                s.push('\n');
                s.into_bytes()
            }
            Err(_) => bytes.to_vec(),
        },
        Err(_) => bytes.to_vec(),
    };

    let mut f = fs::File::create(&tmp)?;
    f.write_all(&to_write)?;
    f.sync_all()?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn write_text_atomic(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    // create a temp filename next to the target by appending .tmp to the filename
    let mut tmp = path.to_path_buf();
    if let Some(fname) = path.file_name().and_then(|n| n.to_str()) {
        tmp.set_file_name(format!("{}.tmp", fname));
    }
    let mut f = fs::File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    fs::rename(tmp, path)?;
    Ok(())
}
