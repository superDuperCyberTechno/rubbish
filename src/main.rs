use axum::{http::HeaderMap, response::IntoResponse, routing::post, Router};
// axum 0.8 does not export `Server` at the crate root; we'll use hyper::Server with the axum service
use hyper::server::Server as HyperServer;
use chrono::Utc;
use std::{fs, io::Write, net::SocketAddr};
use tokio::signal;
use tracing::{error, info};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let app = Router::new().route("/dump", post(handle_dump));

    let addr = SocketAddr::from(([127, 0, 0, 1], 7771));
    info!(%addr, "starting rubbish dump server");

    // axum provides a helper to run the service via hyper; construct the hyper server
    let server = HyperServer::bind(&addr).serve(app.into_make_service());

    // Run server until ctrl-c
    tokio::select! {
        res = server => if let Err(e) = res { error!(%e, "server error"); },
        _ = signal::ctrl_c() => info!("shutting down"),
    }
}

async fn handle_dump(headers: HeaderMap, body: axum::body::Bytes) -> impl IntoResponse {
    // Ensure dumps directory exists
    if let Err(e) = fs::create_dir_all("dumps") {
        error!(%e, "failed to create dumps dir");
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "failed to create dumps dir");
    }

    let ts = Utc::now();
    let filename = make_filename(&headers, &ts);
    let path = format!("dumps/{}.json", filename);

    match save_bytes(&path, &body).await {
        Ok(_) => {
            info!(file = %path, "saved dump");
            (axum::http::StatusCode::OK, "ok")
        }
        Err(e) => {
            error!(%e, "failed to write dump");
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "write failed")
        }
    }
}

fn make_filename(headers: &HeaderMap, ts: &chrono::DateTime<Utc>) -> String {
    let ts = ts.format("%Y%m%dT%H%M%S%.3fZ");
    let title = headers
        .get("rubbish-title")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let title = sanitize_title(title);
    if title.is_empty() {
        format!("{}", ts)
    } else {
        format!("{}_{}", ts, title)
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

async fn save_bytes(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    // write atomically: write to tmp then rename
    let tmp = format!("{}.tmp", path);
    let mut f = fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(tmp, path)?;
    Ok(())
}
