# rubbish (dump server)

This is a small Rust-based dump server that listens on localhost:7771 and accepts JSON dumps POSTed to `/dump`.

Usage:

1. Install Rust toolchain (stable) and run `cargo run --release`.
2. Send JSON to the server: `curl -X POST -H "Content-Type: application/json" -H "rubbish-title: My Dump" --data @payload.json http://127.0.0.1:7771/dump`.
3. Received dumps are saved to the `dumps/` directory with a timestamped filename. If the `rubbish-title` header is provided it is included in the filename.

Notes / next steps:

- Add a TUI to browse saved dumps and open them with `jless` (planned).
- Ensure file rotation / storage limits as needed.
