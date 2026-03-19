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

    // build filename as: [ulid].json (dump name is ULID)
    let id = ulid::Ulid::new().to_string();
    let path = dumps_dir.join(format!("{}.json", id));

    match save_bytes(&path, &body).await {
        Ok(_) => {
            // Build metadata object with optional title and tags and write atomically next to the dump
            // read header value case-insensitively and tolerate non-ideal header names
            fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
                let target = name.to_ascii_lowercase();
                for (k, v) in headers.iter() {
                    let mut kstr = k.as_str().to_ascii_lowercase();
                    // normalize underscore variants
                    kstr = kstr.replace('_', "-");
                    if kstr == target {
                        // try to decode as utf8, but fall back to lossy conversion
                        let val = v.to_str().map(|s| s.to_string()).unwrap_or_else(|_| String::from_utf8_lossy(v.as_bytes()).into_owned());
                        return Some(val);
                    }
                }
                None
            }

            let title_str = header_value(&headers, "rubbish-title").unwrap_or_default();

            let mut tags_vec: Vec<String> = Vec::new();
            if let Some(tags_str) = header_value(&headers, "rubbish-tags") {
                for t in tags_str.split(',') {
                    let tt = t.trim();
                    if !tt.is_empty() {
                        tags_vec.push(tt.to_string());
                    }
                }
            }

            let meta = serde_json::json!({
                "title": title_str,
                "tags": tags_vec,
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

fn make_filename(headers: &HeaderMap, id: &str) -> String {
    // legacy helper retained but now filenames are ULID-only; we still keep the function
    id.to_string()
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
