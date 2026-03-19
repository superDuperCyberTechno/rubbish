// TUI for browsing dumps
// This file was renamed from tui_tui.rs to tui.rs
// (contents unchanged)

#![allow(clippy::needless_return)]

use std::io;
use std::io::Read;
use std::{fs, process::{Command, Stdio}, time::{SystemTime, Duration}, env};
// networking imports removed — server is launched as a child owned by the TUI
use std::path::PathBuf;
// mpsc types used directly where needed
use chrono::{DateTime, Local, TimeZone};
use crossterm::{execute, terminal::{enable_raw_mode, disable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}};
use crossterm::event::{self, Event as CEvent, KeyCode};
use signal_hook::consts::signal::*;
use signal_hook::iterator::Signals;
use std::sync::mpsc::channel;
use std::thread;
use std::collections::{HashMap, HashSet};
use atty::Stream;
use tui::backend::CrosstermBackend;
use tui::Terminal;
use tui::widgets::{Block, Borders, Paragraph, Table, Row, Cell, Widget};
use tui::buffer::Buffer;
use tui::layout::Rect;
use tui::style::{Style, Modifier};
use tui::layout::{Layout, Constraint, Direction};
use tui::widgets::TableState;
use unicode_width::UnicodeWidthStr;

// Render tags with precise placement: each tag line shows tag (left) and a count flush to the
// right edge of the provided area. This avoids table cell padding issues and guarantees
// the count is adjacent to the right border.
struct RawTags<'a> {
    tags: &'a [String],
    counts: &'a [usize],
    selected_tags: &'a HashSet<String>,
    focus: bool,
    tags_selected: Option<usize>,
}

impl<'a> Widget for RawTags<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut y = area.y as u16;
        let max_lines = area.height as usize;
        let max_width = area.width as usize;
        for (i, tag) in self.tags.iter().enumerate() {
            if i >= max_lines { break; }
            let count = self.counts.get(i).copied().unwrap_or(0);
            let count_str = format!("{}", count);
            let count_w = UnicodeWidthStr::width(count_str.as_str());

            // prepare prefix marker
            let prefix = if self.selected_tags.contains(tag) { "> " } else { "" };
            let prefix_w = UnicodeWidthStr::width(prefix);

            // compute available width for tag text so it doesn't overlap the count
            let avail_for_tag = if max_width > count_w { max_width - count_w } else { 0 };
            let avail_for_tag = avail_for_tag.saturating_sub(prefix_w);

            // truncate tag to fit
            let mut display_tag = String::new();
            let mut cur_w = 0usize;
            for ch in tag.chars() {
                let cw = UnicodeWidthStr::width(ch.to_string().as_str());
                if cur_w + cw > avail_for_tag { break; }
                display_tag.push(ch);
                cur_w += cw;
            }
            if display_tag.len() < tag.len() && avail_for_tag >= 3 {
                // add ellipsis if truncated
                if cur_w + 3 <= avail_for_tag {
                    display_tag.push_str("...");
                }
            }

            let left_text = format!("{}{}", prefix, display_tag);

            // determine styles: highlight if focused and this tag is selected index
            let mut style = Style::default();
            let highlighted = if self.focus {
                if let Some(sel) = self.tags_selected { sel == i } else { false }
            } else { false };
            if highlighted {
                style = style.add_modifier(Modifier::REVERSED);
            }

            // If highlighted, fill the entire row with the highlight style so the whole
            // line (tag, padding, count) appears selected.
            if highlighted {
                let fill = std::iter::repeat(' ').take(max_width).collect::<String>();
                buf.set_stringn(area.x, y, &fill, max_width, style);
            }

            // write left_text at left edge (will appear on top of fill if highlighted)
            buf.set_stringn(area.x, y, &left_text, max_width, style);

            // compute x for count so its last cell is at area.x + area.width - 1 using displayed width
            let x_count = area.x + (area.width as u16).saturating_sub(count_w as u16);
            // write count (no truncation needed)
            buf.set_stringn(x_count, y, &count_str, count_str.len(), style);

            y += 1;
        }
    }
}

fn read_preview(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    // Read up to 64KB for preview and return the file contents as-is
    // (do not reformat/pretty-print JSON here; show the file text exactly as stored).
    let f = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(f);
    let mut buf = String::new();
    reader.take(64 * 1024).read_to_string(&mut buf)?;
    Ok(buf)
}

// A custom widget that renders raw preview text without any wrapping. Each line is truncated
// to the available width and written directly to the buffer, so there is no word-wrapping.
struct RawPreview<'a> {
    text: &'a str,
}

impl<'a> Widget for RawPreview<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut y = area.y as u16;
        let max_lines = area.height as usize;
        let max_width = area.width as usize;
        for (i, line) in self.text.lines().enumerate() {
            if i >= max_lines { break; }
            // normalize tabs
            let line = line.replace('\t', "    ");
            // truncate by displayed width (unicode-aware)
            let mut acc = String::new();
            let mut cur_w = 0usize;
            for ch in line.chars() {
                let cw = UnicodeWidthStr::width(ch.to_string().as_str());
                if cur_w + cw > max_width { break; }
                acc.push(ch);
                cur_w += cw;
            }
            // replace spaces with NBSPs so the paragraph renderer won't re-wrap
            let out = acc.replace(' ', "\u{00A0}");
            buf.set_stringn(area.x, y, &out, max_width, Style::default());
            y += 1;
        }
    }
}

fn human_size(bytes: u64) -> String {
    // Use IEC (binary) units: KiB, MiB, GiB, TiB, PiB (base 1024)
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KIB {
        format!("{} B", bytes)
    } else if b < KIB.powi(2) {
        format!("{:.1} KiB", b / KIB)
    } else if b < KIB.powi(3) {
        format!("{:.1} MiB", b / KIB.powi(2))
    } else if b < KIB.powi(4) {
        format!("{:.1} GiB", b / KIB.powi(3))
    } else if b < KIB.powi(5) {
        format!("{:.1} TiB", b / KIB.powi(4))
    } else {
        format!("{:.1} PiB", b / KIB.powi(5))
    }
}

// Truncate a string to fit within `max_w` display width (unicode-aware). If truncation
// occurs, append "...". `max_w` is in display cells.
// NOTE: truncate_display removed — titles are no longer shown in the Dumps box.

// Natural-like comparator: compare numeric runs numerically, otherwise case-insensitive.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        if ai.peek().is_none() && bi.peek().is_none() { return Ordering::Equal; }
        if ai.peek().is_none() { return Ordering::Less; }
        if bi.peek().is_none() { return Ordering::Greater; }
        if ai.peek().unwrap().is_ascii_digit() && bi.peek().unwrap().is_ascii_digit() {
            let mut an = String::new();
            while ai.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) { an.push(ai.next().unwrap()); }
            let mut bn = String::new();
            while bi.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) { bn.push(bi.next().unwrap()); }
            let ai_num = an.trim_start_matches('0').parse::<u128>().ok().unwrap_or(0);
            let bi_num = bn.trim_start_matches('0').parse::<u128>().ok().unwrap_or(0);
            if ai_num != bi_num { return ai_num.cmp(&bi_num); }
            continue;
        }
        let ac = ai.next().unwrap();
        let bc = bi.next().unwrap();
        let acu = ac.to_ascii_lowercase();
        let bcu = bc.to_ascii_lowercase();
        if acu != bcu { return acu.cmp(&bcu); }
    }
}

// (filter_indices removed — use filter_indices_mode for both match_all and match_any)
// Supports two modes: match_all = true means a dump must contain all selected tags (intersection).
// match_all = false means a dump is included if it contains any selected tag (union).
fn filter_indices_mode(selected_tags: &HashSet<String>, tags_vec: &Vec<Vec<String>>, match_all: bool) -> Vec<usize> {
    if selected_tags.is_empty() {
        return (0..tags_vec.len()).collect();
    }
    let need: Vec<&String> = selected_tags.iter().collect();
    let mut out: Vec<usize> = Vec::new();
    for (i, tv) in tags_vec.iter().enumerate() {
        if match_all {
            let mut ok = true;
            for t in need.iter() {
                if !tv.iter().any(|x| x == *t) { ok = false; break; }
            }
            if ok { out.push(i); }
        } else {
            // match any
            let mut ok = false;
            for t in need.iter() {
                if tv.iter().any(|x| x == *t) { ok = true; break; }
            }
            if ok { out.push(i); }
        }
    }
    out
}

// right_align is unused after removing the size column from the table

fn scan_dumps(dumps_dir: &std::path::Path) -> (Vec<(String, String, String)>, Vec<std::path::PathBuf>, Vec<Vec<String>>) {
    // Collect dumps and prefer a server-provided metadata timestamp for ordering when available.
    // We read the metadata for each file up-front so we can sort by the metadata timestamp
    // (fallback to file mtime when metadata timestamp is absent).
    let mut files: Vec<(std::path::PathBuf, SystemTime, std::fs::Metadata, String, Vec<String>, Option<i64>)> = Vec::new();
    if let Ok(rd) = fs::read_dir(dumps_dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let path = e.path();
            if !path.is_file() {
                continue;
            }
            if let Some(fname) = path.file_name().and_then(|s| s.to_str()) {
                if !fname.ends_with(".json") || fname.ends_with(".metadata.json") {
                    continue;
                }
            }
            if let Ok(meta) = e.metadata() {
                let file_mtime = meta.modified().ok().unwrap_or(SystemTime::UNIX_EPOCH);
                // read metadata for title/tags/timestamp
                let (title, tags, meta_ts) = read_metadata_for_path(&path, &dumps_dir);
                // compute effective time: prefer meta_ts when available
                let effective = if let Some(sts) = meta_ts {
                    if sts >= 0 {
                        SystemTime::UNIX_EPOCH + Duration::from_secs(sts as u64)
                    } else {
                        SystemTime::UNIX_EPOCH
                    }
                } else {
                    file_mtime
                };
                files.push((path, effective, meta, title, tags, meta_ts));
            }
        }
    }

    // sort newest first by effective timestamp
    files.sort_by_key(|(_, eff, ..)| *eff);
    files.reverse();

    let mut entries: Vec<(String, String, String)> = Vec::new();
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    let mut tags_vec: Vec<Vec<String>> = Vec::new();
    for (path, effective, meta, title, tags, meta_ts) in files.into_iter() {
        // prefer explicit metadata timestamp if present, otherwise fall back to the file mtime
        let ts_string = if let Some(sts) = meta_ts {
            // construct a Local DateTime from unix seconds in a non-deprecated way
            if let Some(dt) = Local.timestamp_opt(sts, 0).single() {
                dt.format("%Y-%m-%d %H:%M:%S").to_string()
            } else {
                DateTime::<Local>::from(effective).format("%Y-%m-%d %H:%M:%S").to_string()
            }
        } else {
            // effective is either file mtime or converted meta timestamp; if meta_ts absent, effective==file mtime
            DateTime::<Local>::from(effective).format("%Y-%m-%d %H:%M:%S").to_string()
        };

        let size_str = human_size(meta.len());
        entries.push((ts_string, title, size_str));
        paths.push(path.clone());
        tags_vec.push(tags);
    }

    (entries, paths, tags_vec)
}

fn get_mtime(path: &std::path::Path) -> Option<SystemTime> {
    path.metadata().and_then(|m| m.modified()).ok()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Tags,
    Dumps,
}

// Build display entry (timestamp, title, size_str) and return mtime for sorting.
fn build_entry_from_path(path: &std::path::Path) -> Option<((String, String, String), SystemTime)> {
    if !path.is_file() {
        return None;
    }
    // skip metadata sidecar files
    if let Some(fname) = path.file_name().and_then(|s| s.to_str()) {
        if fname.ends_with(".metadata.json") {
            return None;
        }
    }
    let meta = path.metadata().ok()?;
    let mtime = meta.modified().ok()?;
    let size = meta.len();

    let fname = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ts = DateTime::<Local>::from(mtime).format("%Y-%m-%d %H:%M:%S").to_string();

    // extract title from filename [title]_[id].json
    let mut base = fname.clone();
    if base.ends_with(".json") {
        base.truncate(base.len() - 5);
    }
    let title = if let Some(idx) = base.rfind('_') {
        let t = &base[..idx];
        if t.is_empty() { "".to_string() } else { t.to_string() }
    } else {
        "".to_string()
    };

    let size_str = human_size(size);
    Some(((ts, title, size_str), mtime))
}

// Read metadata (title, tags) corresponding to a dump file path.
fn read_metadata_for_path(path: &std::path::Path, dumps_dir: &std::path::Path) -> (String, Vec<String>, Option<i64>) {
    // returns (title, tags, optional_unix_ts_seconds)
    let mut title = String::new();
    let mut tags: Vec<String> = Vec::new();
    let mut timestamp: Option<i64> = None;
    if let Some(fname) = path.file_name().and_then(|s| s.to_str()) {
        if fname.ends_with(".json") {
            let id = &fname[..fname.len() - 5];
            let meta_path = dumps_dir.join(format!("{}.metadata.json", id));
            if let Ok(s) = std::fs::read_to_string(&meta_path) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                    if let Some(t) = v.get("title").and_then(|x| x.as_str()) {
                        title = t.to_string();
                    }
                    if let Some(arr) = v.get("tags").and_then(|x| x.as_array()) {
                        for it in arr.iter() {
                            if let Some(tsv) = it.as_str() {
                                tags.push(tsv.to_string());
                            }
                        }
                    }
                    // prefer an explicit unix timestamp field in metadata
                    if let Some(tv) = v.get("timestamp") {
                        // accept number or numeric string; detect milliseconds vs seconds
                        if let Some(n) = tv.as_i64() {
                            timestamp = Some(n);
                        } else if let Some(un) = tv.as_u64() {
                            // clamp to i64 range
                            timestamp = Some(un as i64);
                        } else if let Some(sv) = tv.as_str() {
                            if let Ok(parsed) = sv.parse::<i64>() {
                                timestamp = Some(parsed);
                            }
                        }
                        // normalize milliseconds to seconds if value seems like millis
                        if let Some(tsv) = timestamp {
                            if tsv.abs() > 3_000_000_000i64 { // > ~ 2065-11-20 in seconds
                                // treat as milliseconds
                                timestamp = Some(tsv / 1000);
                            }
                        }
                    }
                }
            }
        }
    }
    (title, tags, timestamp)
}

fn apply_watch_event(ev: WatchEvent, dumps_dir: &std::path::Path, entries: &mut Vec<(String, String, String)>, paths: &mut Vec<std::path::PathBuf>, tags_vec: &mut Vec<Vec<String>>, selected_tags: &mut HashSet<String>, state: &mut TableState, preview: &mut String) {
    match ev {
        WatchEvent::Rescan => {
            let (new_entries, new_paths, new_tags) = scan_dumps(dumps_dir);
            *entries = new_entries;
            *paths = new_paths;
            *tags_vec = new_tags.clone();
            // rebuild global selected_tags to remove any tags that no longer exist
            let mut all: HashSet<String> = HashSet::new();
            for t in tags_vec.iter().flat_map(|v| v.iter()) {
                all.insert(t.clone());
            }
            selected_tags.retain(|t| all.contains(t));
            if entries.is_empty() {
                *preview = "(no preview available)".to_string();
                state.select(None);
            } else {
                // ensure selection is valid
                match state.selected() {
                    Some(i) if i < entries.len() => {
                        if let Some(p) = paths.get(i) {
                            *preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                        }
                    }
                    _ => {
                        state.select(Some(0));
                        if let Some(p) = paths.get(0) {
                            *preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                        }
                    }
                }
            }
        }
        WatchEvent::Created(p) | WatchEvent::Modified(p) => {
            // ignore events outside the dumps dir
            if !p.starts_with(dumps_dir) {
                return;
            }

            // if file exists, build entry
            if let Some((entry, mtime)) = build_entry_from_path(&p) {
                // read metadata for this path (title/tags/timestamp)
                let (title_meta, tags_meta, meta_ts) = read_metadata_for_path(&p, dumps_dir);
                // if metadata provides a title, override the entry title we built from filename
                let mut real_entry = entry.clone();
                if !title_meta.is_empty() {
                    real_entry.1 = title_meta;
                }
                // if metadata provides an explicit timestamp, prefer it for ordering/display
                let mut use_mtime = mtime;
                if let Some(sts) = meta_ts {
                    // build display timestamp from unix seconds using Local.timestamp_opt
                    if let Some(dt) = Local.timestamp_opt(sts, 0).single() {
                        real_entry.0 = dt.format("%Y-%m-%d %H:%M:%S").to_string();
                    } else {
                        // fallback: keep the entry timestamp we already had
                    }
                    // prefer metadata timestamp when deciding insertion order
                    if sts >= 0 {
                        use_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(sts as u64);
                    }
                }
                // find if path already exists in our list
                if let Some(pos) = paths.iter().position(|x| x == &p) {
                    // update existing entry
                    entries[pos] = real_entry.clone();
                    tags_vec[pos] = tags_meta;
                    // if this is the selected item, refresh preview
                    if state.selected() == Some(pos) {
                        *preview = read_preview(&p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                    }
                } else {
                    // determine insertion index based on effective mtime (newest first)
                    let mut inserted = false;
                    for (i, existing) in paths.iter().enumerate() {
                        // compute existing effective time: if it has metadata, prefer its metadata timestamp when available
                        let existing_meta_ts = read_metadata_for_path(existing, dumps_dir).2;
                        let existing_effective = if let Some(est) = existing_meta_ts {
                            if est >= 0 {
                                SystemTime::UNIX_EPOCH + Duration::from_secs(est as u64)
                            } else {
                                get_mtime(existing).unwrap_or(SystemTime::UNIX_EPOCH)
                            }
                        } else {
                            get_mtime(existing).unwrap_or(SystemTime::UNIX_EPOCH)
                        };
                        if use_mtime > existing_effective {
                            paths.insert(i, p.clone());
                            entries.insert(i, real_entry.clone());
                            tags_vec.insert(i, tags_meta.clone());
                            // if we inserted before the selected index, shift selection down to keep same item
                            if let Some(sel) = state.selected() {
                                if i <= sel {
                                    state.select(Some(sel + 1));
                                }
                            }
                            inserted = true;
                            break;
                        }
                    }
                    if !inserted {
                        paths.push(p.clone());
                        entries.push(real_entry);
                        tags_vec.push(tags_meta);
                    }
                    // if nothing selected, select the first item
                    if state.selected().is_none() {
                        state.select(Some(0));
                        if let Some(first) = paths.get(0) {
                            *preview = read_preview(first).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                        }
                    }
                }
                } else {
                    // file missing or unreadable -> remove if present
                    if let Some(pos) = paths.iter().position(|x| x == &p) {
                        paths.remove(pos);
                        entries.remove(pos);
                        tags_vec.remove(pos);
                        // adjust selection
                        match state.selected() {
                        Some(sel) if sel == pos => {
                            if entries.is_empty() {
                                state.select(None);
                                *preview = "(no preview available)".to_string();
                            } else {
                                let new_sel = if pos == 0 { 0 } else { pos - 1 };
                                state.select(Some(new_sel));
                                if let Some(p2) = paths.get(new_sel) {
                                    *preview = read_preview(p2).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                                }
                            }
                        }
                        Some(sel) if sel > pos => {
                            state.select(Some(sel - 1));
                        }
                        _ => {}
                    }
                }
            }
        }
        WatchEvent::Removed(p) => {
            if let Some(pos) = paths.iter().position(|x| x == &p) {
                paths.remove(pos);
                entries.remove(pos);
                tags_vec.remove(pos);
                        // nothing to do for global selected_tags
                match state.selected() {
                    Some(sel) if sel == pos => {
                        if entries.is_empty() {
                            state.select(None);
                            *preview = "(no preview available)".to_string();
                        } else {
                            let new_sel = if pos == 0 { 0 } else { pos - 1 };
                            state.select(Some(new_sel));
                            if let Some(p2) = paths.get(new_sel) {
                                *preview = read_preview(p2).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                            }
                        }
                    }
                    Some(sel) if sel > pos => {
                        state.select(Some(sel - 1));
                    }
                    _ => {}
                }
            }
        }
    }
}

#[derive(Debug)]
enum WatchEvent {
    Created(std::path::PathBuf),
    Modified(std::path::PathBuf),
    Removed(std::path::PathBuf),
    Rescan,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // start the server as a child process owned by the TUI so it stops when the TUI exits.
    // Prefer a compiled binary in target/debug, otherwise use `cargo run --bin rubbish`.
    let server_child: Option<std::process::Child> = {
        // determine log directory: prefer XDG_DATA_HOME/rubbish, otherwise ~/.local/share/rubbish
        let mut log_dir: PathBuf = match env::var("XDG_DATA_HOME") {
            Ok(x) if !x.is_empty() => PathBuf::from(x).join("rubbish"),
            _ => match env::var("HOME") {
                Ok(h) => PathBuf::from(h).join(".local").join("share").join("rubbish"),
                Err(_) => PathBuf::from("."),
            },
        };
        // Ensure directory exists; fall back to current dir on failure
        if let Err(e) = fs::create_dir_all(&log_dir) {
            eprintln!("warning: failed to create log dir {}: {}\nFalling back to current directory", log_dir.display(), e);
            log_dir = PathBuf::from(".");
        }
        let log_path = log_dir.join("server.log");

        // Try to open a server log file for stdout/stderr capture. Fall back to null if unavailable.
        let (stdout_dest, stderr_dest) = match fs::OpenOptions::new().create(true).append(true).open(&log_path) {
            Ok(f) => match f.try_clone() {
                Ok(f2) => {
                    eprintln!("server logs -> {}", log_path.display());
                    (Stdio::from(f), Stdio::from(f2))
                }
                Err(_) => {
                    eprintln!("warning: failed to clone server log file; disabling logging");
                    (Stdio::null(), Stdio::null())
                }
            },
            Err(_) => {
                eprintln!("warning: failed to open server log file '{}'; logging disabled", log_path.display());
                (Stdio::null(), Stdio::null())
            }
        };

        // Try several candidate server executables in order before falling back to `cargo run`.
        let candidates = [
            "./target/debug/rubbish",
            "./target/release/rubbish",
            "./rubbish",
            "rubbish",
        ];
        let mut server_spawn = Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no candidate tried"));
        for c in &candidates {
            let cmd = std::path::Path::new(c);
            if !cmd.exists() {
                continue;
            }
            // open fresh log file handles for each spawn attempt
            match fs::OpenOptions::new().create(true).append(true).open(log_path) {
                Ok(f1) => {
                    match f1.try_clone() {
                        Ok(f2) => {
                            server_spawn = Command::new(c).stdout(Stdio::from(f1)).stderr(Stdio::from(f2)).spawn();
                        }
                        Err(_) => {
                            server_spawn = Command::new(c).stdout(Stdio::from(f1)).stderr(Stdio::null()).spawn();
                        }
                    }
                }
                Err(_) => {
                    server_spawn = Command::new(c).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
                }
            }
            if server_spawn.is_ok() { break; }
        }
        if server_spawn.is_err() {
            // final fallback: use `cargo run --bin rubbish` and attach the prepared dests
            server_spawn = Command::new("cargo").args(["run","--bin","rubbish"]).stdout(stdout_dest).stderr(stderr_dest).spawn();
        }

        let res = match server_spawn {
            Ok(child) => {
                let pid = child.id();
                eprintln!("started rubbish server (pid={})", pid);
                Some(child)
            }
            Err(e) => {
                eprintln!("failed to start rubbish server: {}", e);
                None
            }
        };

        // give server a moment to bind
        std::thread::sleep(Duration::from_millis(300));
        res
    };

    // determine dumps directory (XDG_DATA_HOME/rubbish/dumps or ~/.local/share/rubbish/dumps)
    let mut dumps_dir: PathBuf = match env::var("XDG_DATA_HOME") {
        Ok(x) if !x.is_empty() => PathBuf::from(x).join("rubbish").join("dumps"),
        _ => match env::var("HOME") {
            Ok(h) => PathBuf::from(h).join(".local").join("share").join("rubbish").join("dumps"),
            Err(_) => PathBuf::from("./dumps"),
        },
    };

    // Ensure dumps directory exists; if creation fails, fall back to ./dumps
    if let Err(e) = fs::create_dir_all(&dumps_dir) {
        eprintln!("warning: failed to create dumps dir {}: {}\nFalling back to ./dumps", dumps_dir.display(), e);
        dumps_dir = PathBuf::from("./dumps");
        let _ = fs::create_dir_all(&dumps_dir);
    }

    // collect files with metadata and sort by modified time (newest first)
    let mut files: Vec<(std::path::PathBuf, Option<SystemTime>, u64)>;
    if let Ok(rd) = fs::read_dir(&dumps_dir) {
        files = rd
            .filter_map(|e| e.ok())
            .map(|e| {
                let path = e.path();
                if !path.is_file() {
                    return None;
                }
                // skip metadata sidecars
                if let Some(fname) = path.file_name().and_then(|s| s.to_str()) {
                    if fname.ends_with(".metadata.json") {
                        return None;
                    }
                }
                let meta = path.metadata().ok();
                let mtime = meta.as_ref().and_then(|m| m.modified().ok());
                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                Some((path, mtime, size))
            })
            .filter_map(|x| x)
            .collect();
    } else {
        // directory missing or unreadable — continue with empty files list so TUI still runs
        files = Vec::new();
    }

    // do not exit when there are no dumps; allow TUI to run with an empty list

    files.sort_by_key(|(_, mtime, _)| mtime.unwrap_or(SystemTime::UNIX_EPOCH));
    files.reverse();

    // entries: (timestamp, title, size_str)
    let mut entries: Vec<(String, String, String)> = Vec::new();
    let mut paths = Vec::new();
    // parallel vector holding tags for each path (may be empty)
    let mut tags_vec: Vec<Vec<String>> = Vec::new();
    // global selected tags across all dumps
    let mut selected_tags: HashSet<String> = HashSet::new();
            for (path, mtime, size) in files.iter() {
                let fname = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let ts = mtime.as_ref().map(|t| DateTime::<Local>::from(*t).format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_else(|| "unknown".into());
        // derive title from metadata if available, otherwise fall back to filename format: [title]_[id].json
        let (meta_title, _meta_tags, _meta_ts) = read_metadata_for_path(path, &dumps_dir);
        let title = if !meta_title.is_empty() {
            meta_title
        } else {
            let mut base = fname.clone();
            if base.ends_with(".json") {
                base.truncate(base.len() - 5);
            }
            if let Some(idx) = base.rfind('_') {
                let t = &base[..idx];
                if t.is_empty() { "".to_string() } else { t.to_string() }
            } else {
                "".to_string()
            }
        };

        let size_str = human_size(*size);
        entries.push((ts, title, size_str));
        paths.push(path.clone());
        // attempt to read metadata for tags
        let (_t, tg, _meta_ts) = read_metadata_for_path(path, &dumps_dir);
        tags_vec.push(tg);
    }

    // initial preview for selected item (used by TTY and non-TTY flows)
    let mut preview = String::new();
    if let Some(p) = paths.get(0) {
        preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
    }
    if entries.is_empty() {
        preview = "(no preview available)".to_string();
    }

    // filesystem watcher: notify main thread when dumps_dir content changes
    // Use a typed WatchEvent channel so we can send granular events to the UI.
    let (fs_tx, fs_rx) = std::sync::mpsc::channel::<WatchEvent>();
    let watch_dir = dumps_dir.clone();

    // Start a notify-based watcher. Translate notify events into WatchEvent values and send them
    // to the TUI thread. If notify fails at runtime, spawn a polling fallback.
    {
        let tx = fs_tx.clone();
        let watch_dir2 = watch_dir.clone();
        thread::spawn(move || {
            use notify::{Watcher, RecursiveMode, RecommendedWatcher, EventKind};
            use std::sync::mpsc::RecvTimeoutError;

            if let Err(e) = (|| -> Result<(), Box<dyn std::error::Error>> {
                let (local_tx, rx) = std::sync::mpsc::channel();
                let mut watcher: RecommendedWatcher = RecommendedWatcher::new(local_tx, notify::Config::default())?;
                watcher.watch(&watch_dir2, RecursiveMode::NonRecursive)?;

                loop {
                    match rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(Ok(ev)) => {
                            for p in ev.paths.iter() {
                                let we = match &ev.kind {
                                    EventKind::Create(_) => WatchEvent::Created(p.clone()),
                                    EventKind::Modify(_) => WatchEvent::Modified(p.clone()),
                                    EventKind::Remove(_) => WatchEvent::Removed(p.clone()),
                                    _ => WatchEvent::Rescan,
                                };
                                let _ = tx.send(we);
                            }
                        }
                        Ok(Err(e)) => {
                            eprintln!("notify event error: {}", e);
                            let _ = tx.send(WatchEvent::Rescan);
                        }
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(_) => break,
                    }
                }

                Ok(())
            })() {
                eprintln!("notify-based watcher failed: {} - falling back to polling", e);

                // Fallback polling implementation in case notify fails at runtime
                let tx2 = tx.clone();
                let watch_dir3 = watch_dir2.clone();
                std::thread::spawn(move || {
                    let mut last: HashMap<String, std::time::SystemTime> = HashMap::new();
                    loop {
                        let mut current: HashMap<String, std::time::SystemTime> = HashMap::new();
                        if let Ok(rd) = std::fs::read_dir(&watch_dir3) {
                            for e in rd.filter_map(|e| e.ok()) {
                                let p = e.path();
                                if p.is_file() {
                                    if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                                        current.insert(p.to_string_lossy().to_string(), m);
                                    }
                                }
                            }
                        }
                        if current != last {
                            for (k, m) in current.iter() {
                                if !last.contains_key(k) {
                                    let _ = tx2.send(WatchEvent::Created(std::path::PathBuf::from(k)));
                                } else if last.get(k).map(|t| t != m).unwrap_or(false) {
                                    let _ = tx2.send(WatchEvent::Modified(std::path::PathBuf::from(k)));
                                }
                            }
                            for k in last.keys() {
                                if !current.contains_key(k) {
                                    let _ = tx2.send(WatchEvent::Removed(std::path::PathBuf::from(k)));
                                }
                            }
                            last = current;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                    }
                });
            }
        });
    }

    // If stdout is not a TTY, fall back to a simple non-interactive listing + preview
    if !atty::is(Stream::Stdout) {
        println!("Dumps (from {}):", dumps_dir.display());
        if entries.is_empty() {
            println!("(no dumps found)");
        } else {
            for (i, (ts, title, size_str)) in entries.iter().enumerate() {
                let display_title = if title.len() > 40 { format!("{}...", &title[..37]) } else { title.clone() };
                if display_title.is_empty() {
                    // omit the title column when it's empty to avoid extra whitespace
                    println!("{}: {:<19} {:>8}", i + 1, ts, size_str);
                } else {
                    println!("{}: {:<19}  {:<40} {:>8}", i + 1, ts, display_title, size_str);
                }
            }
        }
        println!("\n--- Preview (first item) ---\n");
        if preview.is_empty() {
            println!("(no preview available)");
        } else {
            println!("{}", preview);
        }
        return Ok(());
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // interactive list state
    let mut state = TableState::default();
    if entries.is_empty() {
        state.select(None);
    } else {
        state.select(Some(0));
    }

    // tags selection index: start at 0 for the first dump if it has tags
    let mut tags_selected: Option<usize> = None;
    // focus (Tags or Dumps). Default to Dumps
    let mut focus: Focus = Focus::Dumps;
    // filtering mode: true = match all selected tags (intersection), false = match any (union)
    let mut match_all: bool = true;

    // detect whether a pager is available in PATH. Prefer `jless`, but fall back to
    // `less` if jless is not present. If neither is available, Enter will be a no-op.
    let pager_cmd: Option<String> = if Command::new("jless").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false) {
        Some("jless".to_string())
    } else if Command::new("less").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false) {
        Some("less".to_string())
    } else {
        None
    };

    // Setup signal handling to gracefully exit and restore terminal
    let (sig_tx, sig_rx) = channel();
    let mut signals = Signals::new(&[SIGINT, SIGTERM, SIGQUIT]).unwrap();
    thread::spawn(move || {
        for sig in signals.forever() {
            // send the signal number to the main thread
            let _ = sig_tx.send(sig);
        }
    });

    // render loop
    loop {
        // handle filesystem events (non-blocking) and apply incremental updates
        if let Ok(ev) = fs_rx.try_recv() {
            apply_watch_event(ev, &dumps_dir, &mut entries, &mut paths, &mut tags_vec, &mut selected_tags, &mut state, &mut preview);
        }
        // Build a global unique sorted tag list for the Tags box
        let mut uniq_set: HashSet<String> = HashSet::new();
        for v in tags_vec.iter() {
            for t in v.iter() {
                uniq_set.insert(t.clone());
            }
        }
        let mut unique_tags: Vec<String> = uniq_set.into_iter().collect();
        // natural-like sort: split numeric runs and compare numerically when possible
        unique_tags.sort_by(|a, b| natural_cmp(a, b));
        // ensure tags_selected is valid for unique_tags; default to first tag when absent
        if unique_tags.is_empty() {
            tags_selected = None;
        } else if tags_selected.is_none() {
            tags_selected = Some(0);
        } else if let Some(idx) = tags_selected {
            if idx >= unique_tags.len() {
                tags_selected = Some(0);
            }
        }

        let display_indices: Vec<usize> = filter_indices_mode(&selected_tags, &tags_vec, match_all);
        terminal.draw(|f| {
            let size = f.size();
            // reserve one line at the bottom for a full-width status line
            let vchunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(size.height.saturating_sub(1)), Constraint::Length(1)].as_ref())
                .split(size);
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
                .split(vchunks[0]);
            // within the left area (chunks[0]) split into Tags and Dumps.
            // Make the Dumps box a fixed outer width so its inner content width is exactly
            // 20 characters (optional indent + timestamp). Block borders add 2 characters,
            // so set the outer length to 22.
            let left_chunks = Layout::default()
                .direction(Direction::Horizontal)
                // Outer length = inner width (21) + 2 for block borders
                .constraints([Constraint::Min(10), Constraint::Length(23)].as_ref())
                .split(chunks[0]);

            // Render a table with a single column: timestamp. Titles are omitted from the Dumps box.
            let rows: Vec<Row> = display_indices
                .iter()
                .filter_map(|&i| entries.get(i).map(|e| (i, e.clone())))
                .map(|(i, (ts, _title, _size_str))| {
                    // indicator column: '>' for the currently selected master index, otherwise space
                    // Only indent the focused dump's timestamp. No visible indicator is shown;
                    // focused rows get one leading space, others have none.
                    // focused dump should be offset two spaces to the right; non-focused
                    // dumps are unindented. Prefix is applied only for the selected row.
                    let prefix = if Some(i) == state.selected() { "  " } else { "" };
                    let cell = format!("{}{}", prefix, ts);
                Row::new(vec![Cell::from(cell)])
                })
                .collect();

                let table_block = Block::default().borders(Borders::ALL).title("Dumps");
                let table = Table::new(rows)
                .block(table_block.clone())
                .widths(&[Constraint::Length(21)])
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

            if entries.is_empty() {
                // draw an empty placeholder in the table area when there are no dumps at all
                let empty = Paragraph::new("(no dumps found)").block(Block::default().borders(Borders::ALL).title("Dumps"));
                f.render_widget(empty, left_chunks[1]);
            } else if display_indices.is_empty() {
                // nothing matches the active tag filter
                let empty = Paragraph::new("(no dumps match selected tags)").block(Block::default().borders(Borders::ALL).title("Dumps"));
                f.render_widget(empty, left_chunks[1]);
            } else {
                // build a temporary TableState that selects the position within the displayed
                // rows corresponding to the underlying `state` selection (which indexes the
                // master entries/paths arrays). This keeps apply_watch_event logic operating
                // on the master indices while allowing correct highlighting of the filtered view.
                let mut display_state = tui::widgets::TableState::default();
                if let Some(master_sel) = state.selected() {
                    if let Some(pos) = display_indices.iter().position(|&x| x == master_sel) {
                        display_state.select(Some(pos));
                    } else {
                        display_state.select(None);
                    }
                } else {
                    display_state.select(None);
                }

                if focus == Focus::Dumps {
                    f.render_stateful_widget(table, left_chunks[1], &mut display_state);
                } else {
                    f.render_widget(table, left_chunks[1]);
                }

                // draw a marker glyph '>' at the leftmost column of the focused display row
                if let Some(master_sel) = state.selected() {
                    if let Some(display_pos) = display_indices.iter().position(|&x| x == master_sel) {
                        // inner area of the table block (where rows are drawn)
                        let inner = table_block.inner(left_chunks[1]);
                        struct Marker;
                        impl Widget for Marker {
                            fn render(self, area: Rect, buf: &mut Buffer) {
                                let y = area.y as u16;
                                buf.set_stringn(area.x, y, ">", 1, Style::default().add_modifier(Modifier::BOLD));
                            }
                        }
                        // limit to visible rows
                        if display_pos < inner.height as usize {
                            let mut area = inner;
                            area.y = inner.y + display_pos as u16;
                            area.height = 1;
                            f.render_widget(Marker, area);
                        }
                    }
                }
            }

            // Use RawPreview to render lines truncated to the available width with no wrapping.
            let preview_widget = RawPreview { text: &preview };
            let block = Block::default().borders(Borders::ALL).title("Preview");
            let inner = block.inner(chunks[1]);
            f.render_widget(block, chunks[1]);
            f.render_widget(preview_widget, inner);

            // Tags box on the left of Dumps — show one entry per unique tag across all dumps
            // Include a right-aligned count of how many dumps have that tag.
            // Render tags as a two-column table: tag (left) and count (right). This allows
            // precise control over the count column width so counts are visible and right-aligned.
            if unique_tags.is_empty() {
                let empty = Paragraph::new("(no tags)").block(Block::default().borders(Borders::ALL).title("Tags"));
                f.render_widget(empty, left_chunks[0]);
            } else {
                // compute counts for each tag
                let mut counts: Vec<usize> = Vec::with_capacity(unique_tags.len());
                for t in unique_tags.iter() {
                    let cnt = tags_vec.iter().filter(|tv| tv.iter().any(|x| x == t)).count();
                    counts.push(cnt);
                }

                // Draw a bordered Tags block and render RawTags inside its inner area so the
                // counts appear flush against the block's right border.
                let tags_block = Block::default().borders(Borders::ALL).title("Tags");
                // render a clone for the block and keep an owned copy to compute inner area
                let tags_block_clone = tags_block.clone();
                f.render_widget(tags_block_clone, left_chunks[0]);
                let inner = tags_block.inner(left_chunks[0]);
                let raw = RawTags { tags: &unique_tags, counts: &counts, selected_tags: &selected_tags, focus: focus == Focus::Tags, tags_selected };
                f.render_widget(raw, inner);
            }

            // status line spanning full width, below the boxes
            // determine selected title and size for status rendering
            let (sel_title, sel_size) = match state.selected().and_then(|i| entries.get(i)) {
                Some((_ts, title, size_str)) => (title.clone(), size_str.clone()),
                None => (String::new(), String::new()),
            };
            // Render status line with title (left) and suffix+size (right). Keep title un-truncated
            // when it fits; otherwise truncate to available space.
            let width = vchunks[1].width as usize;

            // size should be preserved; prefer showing full title and size. Truncate the
            // suffix (selected-tags/mode) first if space is tight. Only truncate title as
            // a last resort.
            let size_w = UnicodeWidthStr::width(sel_size.as_str());
            if size_w >= width {
                // only room for (truncated) size
                let mut acc = String::new();
                let mut cur_w = 0usize;
                for ch in sel_size.chars() {
                    let cw = UnicodeWidthStr::width(ch.to_string().as_str());
                    if cur_w + cw > width { break; }
                    acc.push(ch);
                    cur_w += cw;
                }
                f.render_widget(Paragraph::new(acc), vchunks[1]);
            } else {
                // compute max space for title (reserve 1 space between title and right-side)
                let max_title_w = width.saturating_sub(size_w + 1);
                let title_w = UnicodeWidthStr::width(sel_title.as_str());
                let title_display = if title_w <= max_title_w {
                    sel_title.clone()
                } else if max_title_w >= 4 {
                    // truncate and add ellipsis
                    let mut acc = String::new();
                    let mut cur_w = 0usize;
                    for ch in sel_title.chars() {
                        let cw = UnicodeWidthStr::width(ch.to_string().as_str());
                        if cur_w + cw + 3 > max_title_w { break; }
                        acc.push(ch);
                        cur_w += cw;
                    }
                    acc.push_str("...");
                    acc
                } else {
                    String::new()
                };

                // right side only contains the size (we no longer display tag-filter info here)
                let right_side = sel_size.clone();

                let pad_count = width.saturating_sub(UnicodeWidthStr::width(title_display.as_str()) + UnicodeWidthStr::width(right_side.as_str()));
                let pad = std::iter::repeat('\u{00A0}').take(pad_count).collect::<String>();
                let final_line = format!("{}{}{}", title_display, pad, right_side);
                f.render_widget(Paragraph::new(final_line), vchunks[1]);
            }
        })?;

        // handle input or signals
        if event::poll(Duration::from_millis(200))? {
            if let CEvent::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Esc => break,
                    // vim-style left
                    KeyCode::Left | KeyCode::Char('h') => {
                        focus = Focus::Tags;
                        // when focusing tags, select the first tag of the currently selected dump
                        // within the global unique_tags list so Up/Down navigates the global list.
                        if let Some(dsel) = state.selected() {
                            if let Some(first_tag) = tags_vec.get(dsel).and_then(|v| v.get(0)) {
                                tags_selected = unique_tags.iter().position(|t| t == first_tag);
                                // if not found for some reason, default to 0
                                if tags_selected.is_none() && !unique_tags.is_empty() {
                                    tags_selected = Some(0);
                                }
                            } else {
                                // dump has no tags -> no tag focused
                                tags_selected = None;
                            }
                        } else {
                            tags_selected = None;
                        }
                    }
                    // vim-style right
                    KeyCode::Right | KeyCode::Char('l') => {
                        focus = Focus::Dumps;
                    }
                    // vim-style up
                    KeyCode::Up | KeyCode::Char('k') => {
                        match focus {
                            Focus::Dumps => {
                                // move selection within the filtered view (display_indices)
                                if entries.is_empty() { /* nothing */ } else {
                                    if display_indices.is_empty() {
                                        // no visible items
                                    } else {
                                        // find position of current master selection within display_indices
                                        let cur_pos = state.selected().and_then(|ms| display_indices.iter().position(|&x| x == ms));
                                        let new_pos = match cur_pos {
                                            Some(0) | None => display_indices.len() - 1,
                                            Some(p) => p - 1,
                                        };
                                        let master_idx = display_indices[new_pos];
                                        state.select(Some(master_idx));
                                        if let Some(p) = paths.get(master_idx) {
                                            preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                                        }
                                        // update tags_selected for the newly selected dump
                                        tags_selected = tags_vec.get(master_idx).and_then(|v| if v.is_empty() { None } else { Some(0) });
                                    }
                                }
                            }
                            Focus::Tags => {
                                if let Some(sel) = tags_selected {
                                    let len = unique_tags.len();
                                    if len > 0 {
                                        let ni = if sel == 0 { len - 1 } else { sel - 1 };
                                        tags_selected = Some(ni);
                                    }
                                }
                            }
                        }
                    }
                    // vim-style down
                    KeyCode::Down | KeyCode::Char('j') => {
                        match focus {
                            Focus::Dumps => {
                                // move selection within the filtered view (display_indices)
                                if entries.is_empty() { /* nothing */ } else {
                                    if display_indices.is_empty() {
                                        // no visible items
                                    } else {
                                        let cur_pos = state.selected().and_then(|ms| display_indices.iter().position(|&x| x == ms));
                                        let new_pos = match cur_pos {
                                            None => 0,
                                            Some(p) => (p + 1) % display_indices.len(),
                                        };
                                        let master_idx = display_indices[new_pos];
                                        state.select(Some(master_idx));
                                        if let Some(p) = paths.get(master_idx) {
                                            preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                                        }
                                        tags_selected = tags_vec.get(master_idx).and_then(|v| if v.is_empty() { None } else { Some(0) });
                                    }
                                }
                            }
                           Focus::Tags => {
                                if let Some(sel) = tags_selected {
                                    let len = unique_tags.len();
                                    if len > 0 {
                                        let ni = (sel + 1) % len;
                                        tags_selected = Some(ni);
                                    }
                                }
                            }
                        }
                    }
                    KeyCode::Char(' ') => {
                        // toggle selection of focused tag when Tags has focus
                        if focus == Focus::Tags {
                            if let Some(tsel) = tags_selected {
                                if let Some(tag) = unique_tags.get(tsel) {
                                    if selected_tags.contains(tag) {
                                        selected_tags.remove(tag);
                                    } else {
                                        selected_tags.insert(tag.clone());
                                    }
                                    // after changing the active tag filter, ensure the current master
                                    // selection is visible. If not, move selection to the first visible
                                    // entry (if any) and update preview.
                                    if entries.is_empty() {
                                        // nothing to do
                                    } else {
                                        let new_display: Vec<usize> = if selected_tags.is_empty() { (0..entries.len()).collect() } else {
                                            let need: Vec<&String> = selected_tags.iter().collect();
                                            let mut out: Vec<usize> = Vec::new();
                                            for (i, tv) in tags_vec.iter().enumerate() {
                                                let mut ok = true;
                                                for t in need.iter() { if !tv.iter().any(|x| x == *t) { ok = false; break; } }
                                                if ok { out.push(i); }
                                            }
                                            out
                                        };
                                        if new_display.is_empty() {
                                            state.select(None);
                                            preview = "(no preview available)".to_string();
                                        } else {
                                            // if current selection is not in new_display, pick first
                                            match state.selected() {
                                                Some(ms) if new_display.iter().any(|&x| x == ms) => {
                                                    // keep current selection
                                                }
                                                _ => {
                                                    state.select(Some(new_display[0]));
                                                    if let Some(p) = paths.get(new_display[0]) {
                                                        preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    KeyCode::Char('c') => {
                        // clear all selected tags
                        selected_tags.clear();
                        // restore selection to first item if available
                        if !entries.is_empty() {
                            state.select(Some(0));
                            if let Some(p) = paths.get(0) {
                                preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                            }
                        } else {
                            state.select(None);
                            preview = "(no preview available)".to_string();
                        }
                    }
                    KeyCode::Char('m') => {
                        // toggle matching mode between match-all and match-any
                        match_all = !match_all;
                        // After changing mode, ensure current selection is visible or pick first
                        let new_display = filter_indices_mode(&selected_tags, &tags_vec, match_all);
                        if new_display.is_empty() {
                            state.select(None);
                            preview = "(no preview available)".to_string();
                        } else {
                            match state.selected() {
                                Some(ms) if new_display.iter().any(|&x| x == ms) => {}
                                _ => {
                                    state.select(Some(new_display[0]));
                                    if let Some(p) = paths.get(new_display[0]) {
                                        preview = read_preview(p).unwrap_or_else(|e| format!("failed to read preview: {}", e));
                                    }
                                }
                            }
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(pager) = pager_cmd.clone() {
                            if let Some(i) = state.selected() {
                                let path = paths[i].clone();

                                // restore terminal to normal
                                let _ = disable_raw_mode();
                                let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
                                let _ = terminal.show_cursor();

                                // spawn pager and forward signals while it runs
                                let child = Command::new(pager).arg(&path).spawn();

                                if let Ok(mut child) = child {
                                    loop {
                                        if let Ok(sig) = sig_rx.try_recv() {
                                            unsafe { libc::kill(child.id() as i32, sig); }
                                        }
                                        match child.try_wait() {
                                            Ok(Some(_)) => break,
                                            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                                            Err(_) => break,
                                        }
                                    }
                                }

                                // Recreate terminal backend and re-enter alternate screen
                                let mut stdout = io::stdout();
                                execute!(stdout, EnterAlternateScreen)?;
                                let backend = CrosstermBackend::new(stdout);
                                terminal = Terminal::new(backend)?;
                                let _ = enable_raw_mode();

                                // refresh preview for current selection
                                preview = read_preview(&path).unwrap_or_else(|e| format!("failed to read preview: {}", e));

                                // Small delay then redraw
                                std::thread::sleep(Duration::from_millis(100));
                                let _ = terminal.draw(|f| {
                                    let size = f.size();
                                    let vchunks = Layout::default()
                                        .direction(Direction::Vertical)
                                        .constraints([Constraint::Length(size.height.saturating_sub(1)), Constraint::Length(1)].as_ref())
                                        .split(size);
                                    let chunks = Layout::default()
                                        .direction(Direction::Horizontal)
                                        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
                                        .split(vchunks[0]);
                                    let left_chunks = Layout::default()
                                        .direction(Direction::Horizontal)
                                        .constraints([Constraint::Min(10), Constraint::Length(23)].as_ref())
                                        .split(chunks[0]);

                                    // rebuild table rows
                                    let rows: Vec<Row> = entries
                                        .iter()
                                        .enumerate()
                                        .map(|(i, (ts, _title, _size_str))| {
                                            let prefix = if Some(i) == state.selected() { "  " } else { "" };
                                            let cell = format!("{}{}", prefix, ts);
                                            Row::new(vec![Cell::from(cell)])
                                        })
                                        .collect();

                                    let table_block = Block::default().borders(Borders::ALL).title("Dumps");
                                    let table = Table::new(rows).block(table_block.clone()).widths(&[Constraint::Length(21)]);
                                    f.render_stateful_widget(table, left_chunks[1], &mut state);

                                    let preview_widget = RawPreview { text: &preview };
                                    let block = Block::default().borders(Borders::ALL).title("Preview");
                                    let inner = block.inner(chunks[1]);
                                    f.render_widget(block, chunks[1]);
                                    f.render_widget(preview_widget, inner);

                                    // tags box
                                    let tags_text = match state.selected() {
                                        Some(i) => tags_vec.get(i).map(|v| if v.is_empty() { "(no tags)".to_string() } else { v.join("\n") }).unwrap_or_else(|| "(no tags)".to_string()),
                                        None => "(no tags)".to_string(),
                                    };
                                    let mut tags_block = Paragraph::new(tags_text).block(Block::default().borders(Borders::ALL).title("Tags"));
                                    if focus == Focus::Tags { tags_block = tags_block.style(Style::default().add_modifier(Modifier::REVERSED)); }
                                    f.render_widget(tags_block, left_chunks[0]);

                                    // status
                                    let status = if let Some((_ts, title, size_str)) = state.selected().and_then(|i| entries.get(i)) {
                                        let width = vchunks[1].width as usize;
                                        let size_w = UnicodeWidthStr::width(size_str.as_str());
                                        if size_w >= width {
                                            let mut acc = String::new();
                                            let mut cur_w = 0usize;
                                            for ch in size_str.chars() {
                                                let cw = UnicodeWidthStr::width(ch.to_string().as_str());
                                                if cur_w + cw > width { break; }
                                                acc.push(ch);
                                                cur_w += cw;
                                            }
                                            Paragraph::new(acc)
                                        } else {
                                            let title_w = UnicodeWidthStr::width(title.as_str());
                                            let max_title_w = width.saturating_sub(size_w + 1);
                                            let title_display = if title_w > max_title_w {
                                                let mut acc = String::new();
                                                let mut cur_w = 0usize;
                                                for ch in title.chars() {
                                                    let cw = UnicodeWidthStr::width(ch.to_string().as_str());
                                                    if cur_w + cw + 3 > max_title_w { break; }
                                                    acc.push(ch);
                                                    cur_w += cw;
                                                }
                                                acc.push_str("...");
                                                acc
                                            } else { title.clone() };
                                            let pad_count = width.saturating_sub(UnicodeWidthStr::width(title_display.as_str()) + size_w);
                                            let pad = std::iter::repeat('\u{00A0}').take(pad_count).collect::<String>();
                                            Paragraph::new(format!("{}{}{}", title_display, pad, size_str))
                                        }
                                    } else { Paragraph::new("") };
                                    f.render_widget(status, vchunks[1]);
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // check for signals
        if let Ok(sig) = sig_rx.try_recv() {
            // on signal, break loop and restore terminal
            eprintln!("received signal {} - exiting", sig);
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // Ensure we stop the server child we spawned so the server is not independent of the TUI.
    if let Some(mut child) = server_child {
        // try a graceful shutdown
        let _ = child.kill();
        let _ = child.wait();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_human_size_basic() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(1024 * 1024), "1.0 MiB");
        assert_eq!(human_size(1024_u64.pow(3)), "1.0 GiB");
    }

    #[test]
    fn test_build_entry_from_path_extracts_title_and_size() {
        // create a temporary file in the system temp dir
        let dir = tempdir().expect("tempdir");
        let mut path = dir.path().to_path_buf();
        let fname = format!("2026-03-18_test-title-{}_.json", uuid::Uuid::new_v4());
        path.push(&fname);

        let data = b"{\"x\":1}\n";
        {
            let mut f = fs::File::create(&path).expect("create temp file");
            f.write_all(data).expect("write data");
        }

        // ensure file metadata is available
        std::thread::sleep(std::time::Duration::from_millis(10));

        let res = build_entry_from_path(&path).expect("should build entry");
        let ((ts, title, size_str), _mtime) = res;

        // title should match (stripping .json and underscore logic)
        assert!(title.contains("test-title"));
        // size_str should reflect the bytes we wrote (small -> bytes)
        assert!(size_str.ends_with("B") || size_str.contains("KiB"));
        // ts should be a non-empty timestamp string
        assert!(!ts.is_empty());

        // cleanup
        let _ = fs::remove_file(&path);
        // tempdir drops
    }

    #[test]
    fn test_scan_dumps_ignores_metadata_and_reads_titles() {
        let dir = tempdir().expect("tempdir");
        let dumps_dir = dir.path();

        // create a dump file and a metadata file
        let id = "00000000000000000000000000";
        let dump_path = dumps_dir.join(format!("{}.json", id));
        fs::write(&dump_path, b"{\"a\":1}\n").expect("write dump");
        let meta = serde_json::json!({"title": "Meta Title", "tags": ["t1","t2"]});
        let meta_path = dumps_dir.join(format!("{}.metadata.json", id));
        fs::write(&meta_path, serde_json::to_string(&meta).unwrap()).expect("write meta");

        let (entries, paths, tags_vec) = scan_dumps(dumps_dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(paths.len(), 1);
        assert_eq!(tags_vec.len(), 1);
        let (_ts, title, _size) = &entries[0];
        assert_eq!(title, "Meta Title");
        assert_eq!(tags_vec[0], vec!["t1".to_string(), "t2".to_string()]);
    }

    #[test]
    fn test_filter_indices_basic() {
        let mut tags_vec: Vec<Vec<String>> = Vec::new();
        tags_vec.push(vec!["a".to_string(), "b".to_string()]); // 0
        tags_vec.push(vec!["b".to_string(), "c".to_string()]); // 1
        tags_vec.push(vec!["a".to_string(), "c".to_string()]); // 2

        let mut sel: HashSet<String> = HashSet::new();
        // empty selection -> all indices
        let all = filter_indices_mode(&sel, &tags_vec, true);
        assert_eq!(all, vec![0,1,2]);

        sel.insert("a".to_string());
        let a_idxs = filter_indices_mode(&sel, &tags_vec, true);
        assert_eq!(a_idxs, vec![0,2]);

        sel.insert("b".to_string());
        let ab_idxs = filter_indices_mode(&sel, &tags_vec, true);
        assert_eq!(ab_idxs, vec![0]);

        sel.insert("z".to_string());
        let none = filter_indices_mode(&sel, &tags_vec, true);
        assert!(none.is_empty());
    }

    #[test]
    fn test_filter_indices_mode_union() {
        let mut tags_vec: Vec<Vec<String>> = Vec::new();
        tags_vec.push(vec!["a".to_string(), "b".to_string()]); // 0
        tags_vec.push(vec!["b".to_string(), "c".to_string()]); // 1
        tags_vec.push(vec!["d".to_string()]); // 2

        let mut sel: HashSet<String> = HashSet::new();
        sel.insert("b".to_string());
        let union = filter_indices_mode(&sel, &tags_vec, false);
        assert_eq!(union, vec![0,1]);

        sel.insert("d".to_string());
        let union2 = filter_indices_mode(&sel, &tags_vec, false);
        assert_eq!(union2, vec![0,1,2]);
    }

    #[test]
    fn test_read_metadata_timestamp_seconds_and_millis() {
        let dir = tempdir().expect("tempdir");
        let dumps_dir = dir.path();

        // create a dump file
        let id = "ts_test_id";
        let dump_path = dumps_dir.join(format!("{}.json", id));
        fs::write(&dump_path, b"{}\n").expect("write dump");

        // metadata with timestamp in seconds (should be preserved)
        let ts_secs: i64 = 1_700_000_000; // plausible seconds value
        let meta_secs = serde_json::json!({"title": "S", "tags": ["t"], "timestamp": ts_secs});
        let meta_path = dumps_dir.join(format!("{}.metadata.json", id));
        fs::write(&meta_path, serde_json::to_string(&meta_secs).unwrap()).expect("write meta secs");

        let (_title, _tags, parsed_secs) = read_metadata_for_path(&dump_path, &dumps_dir);
        assert_eq!(parsed_secs, Some(ts_secs));

        // metadata with timestamp in milliseconds (should be normalized to seconds)
        let ts_millis: i64 = ts_secs * 1000;
        let meta_millis = serde_json::json!({"title": "M", "tags": ["t"], "timestamp": ts_millis});
        fs::write(&meta_path, serde_json::to_string(&meta_millis).unwrap()).expect("write meta millis");

        let (_title2, _tags2, parsed_millis) = read_metadata_for_path(&dump_path, &dumps_dir);
        assert_eq!(parsed_millis, Some(ts_secs));

        // metadata with numeric-string milliseconds
        let meta_millis_str = serde_json::json!({"title":"MS","tags":["t"],"timestamp": ts_millis.to_string()});
        fs::write(&meta_path, serde_json::to_string(&meta_millis_str).unwrap()).expect("write meta millis str");

        let (_title3, _tags3, parsed_millis_str) = read_metadata_for_path(&dump_path, &dumps_dir);
        assert_eq!(parsed_millis_str, Some(ts_secs));
    }
}
