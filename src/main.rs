use axum::{extract::RawBody, http::HeaderMap, response::IntoResponse, routing::post, Router};
use chrono::Utc;
use std::{fs, io::Write, net::SocketAddr, path::Path};
use tokio::signal;
use tracing::{error, info};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let app = Router::new().route("/dump", post(handle_dump));

    let addr = SocketAddr::from(([127, 0, 0, 1], 7771));
    info!(%addr, "starting rubbish dump server");

    let server = axum::Server::bind(&addr).serve(app.into_make_service());

    // Run server until ctrl-c
    tokio::select! {
        res = server => if let Err(e) = res { error!(%e, "server error"); },
        _ = signal::ctrl_c() => info!("shutting down"),
    }
}

async fn handle_dump(headers: HeaderMap, RawBody(body): RawBody) -> impl IntoResponse {
    // Ensure dumps directory exists
    if let Err(e) = fs::create_dir_all("dumps") {
        error!(%e, "failed to create dumps dir");
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "failed to create dumps dir");
    }

    let ts = Utc::now();
    let filename = make_filename(&headers, &ts);
    let path = format!("dumps/{}.json", filename);

    // Write body bytes to file
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

    let title = sanitize_filename::sanitize(title);
    if title.is_empty() {
        format!("{}", ts)
    } else {
        format!("{}_{}", ts, title)
    }
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
