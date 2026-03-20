# rubbish (dump server + TUI)

rubbish is a small Rust utility (single binary) that runs an HTTP dump receiver and an interactive terminal UI together. When developing software, you can then dump data to `rubbish` in JSON format and have rubbish filter and display the data for your viewing pleasure.

Key points:
- Single executable: the `rubbish` binary runs both the HTTP server and the interactive TUI; exiting the TUI (q/Esc or Ctrl-C) also shuts down the server.
- Server: binds to `127.0.0.1:7771` and accepts JSON POSTs to `/dump`.
- TUI: full-screen terminal UI (uses the `tui` + `crossterm` crates). The TUI lists received dumps, shows a raw preview, and supports tag filtering and a pager for viewing full dump files.

## Quick start

1. Build and run the combined app:

   - Debug: `cargo run`
   - Release: `cargo run --release`

2. Send a JSON dump to the server (example):

   curl -X POST \
     -H "Content-Type: application/json" \
     -H "rubbish-title: My Dump" \
     -H "rubbish-tags: bug,urgent" \
     --data @payload.json \
     http://127.0.0.1:7771/

Files and metadata

- Dumps are stored under the dumps directory determined by XDG rules:
  - If `XDG_DATA_HOME` is set: `$XDG_DATA_HOME/rubbish/dumps`
  - Otherwise: `$HOME/.local/share/rubbish/dumps` or `./dumps` as a fallback.
- Each dump is saved as `<ULID>.json` and has an accompanying sidecar metadata file `<ULID>.metadata.json` with the following JSON shape:

  {
    "title": "(optional title from header)",
    "tags": ["tag1", "tag2"],
    "timestamp": 1610000000
  }

  - `timestamp` is written by the server as Unix timestamp with millisecond precision.

TUI / pager behavior

- The TUI prefers `jless` as the pager, falls back to `less`, otherwise Enter is a no-op.
- Preview in the TUI shows raw file contents (no wrapping); pressing Enter opens the pager for the selected dump.

Logging / environment

- The application uses `tracing` for logging (goes to stderr by default).
- There is a `RUBBISH_SERVER_LOG` environment variable used historically to control server child logging; for the in-process server logs go to stderr (we can add file logging via env on request).

Version

- Current package version: 1.0.0
