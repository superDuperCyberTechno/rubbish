use axum::{http::HeaderMap, response::IntoResponse, routing::post, Router};
// run with axum's `serve` helper using a TcpListener
use tokio::net::TcpListener;
use chrono::Utc;
use std::{fs, io::Write, net::SocketAddr};
use std::path::PathBuf;
use std::env;
use tokio::signal;
use tracing::{error, info};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let app = Router::new().route("/dump", post(handle_dump));

    let addr = SocketAddr::from(([127, 0, 0, 1], 7771));
    info!(%addr, "starting rubbish dump server");

    // bind a TcpListener and run via axum::serve
    let listener = TcpListener::bind(addr).await.expect("failed to bind");
    let server = axum::serve(listener, app);

    // Run server until ctrl-c
    tokio::select! {
        res = server => if let Err(e) = res { error!(%e, "server error"); },
        _ = signal::ctrl_c() => info!("shutting down"),
    }
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

    // build filename as: [title]_[ulid].json where title may be empty
    let id = ulid::Ulid::new().to_string();
    let filename = make_filename(&headers, &id);
    let path = dumps_dir.join(format!("{}.json", filename));

    match save_bytes(&path, &body).await {
        Ok(_) => {
            // If tags were provided, write a .tags file next to the dump containing the raw tags header
            if let Some(tags_val) = headers.get("rubbish-tags") {
                if let Ok(tags_str) = tags_val.to_str() {
                    let tags_path = path.with_extension("tags");
                    if let Err(e) = write_text_atomic(&tags_path, tags_str) {
                        error!(%e, file = %tags_path.display(), "failed to write tags file");
                    }
                }
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

fn make_filename(headers: &HeaderMap, id: &str) -> String {
    let title = headers
        .get("rubbish-title")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let title = sanitize_title(title);
    if title.is_empty() {
        format!("_{}", id)
    } else {
        format!("{}_{}", title, id)
    }
}

fn sanitize_title(s: &str) -> String {
    // keep alphanumeric, dash, underscore and spaces; convert spaces to underscore;
    // collapse runs and truncate to 60 chars
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c.is_whitespace() {
            if c.is_whitespace() {
                out.push('_');
            } else {
                out.push(c);
            }
        }
    }
    // collapse multiple underscores
    let mut collapsed = String::with_capacity(out.len());
    let mut prev_underscore = false;
    for c in out.chars() {
        if c == '_' {
            if !prev_underscore {
                collapsed.push(c);
            }
            prev_underscore = true;
        } else {
            collapsed.push(c);
            prev_underscore = false;
        }
    }
    let trimmed = collapsed.trim_matches('_');
    let res: String = trimmed.chars().take(60).collect();
    res
}

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
    let tmp = path.with_extension("tags.tmp");
    let mut f = fs::File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    fs::rename(tmp, path)?;
    Ok(())
}
