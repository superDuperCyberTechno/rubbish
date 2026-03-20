// TUI for browsing dumps (moved into library module so `rubbish` can embed it)
#![allow(clippy::needless_return)]

use std::io;
use std::io::Read;
use std::{fs, process::Command, time::{SystemTime, Duration}, env};
use std::path::PathBuf;
use chrono::{DateTime, Local, TimeZone};
use crossterm::{execute, terminal::{enable_raw_mode, disable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}};
use crossterm::event::{self, Event as CEvent, KeyCode};
use signal_hook::consts::signal::*;
use signal_hook::iterator::Signals;
use std::sync::mpsc::channel;
use std::thread;
use std::collections::{HashMap, HashSet};
use tui::backend::CrosstermBackend;
use tui::Terminal;
use tui::widgets::{Block, Borders, Paragraph, Table, Row, Cell, Widget};
use tui::buffer::Buffer;
use tui::layout::Rect;
use tui::style::{Style, Modifier};
use tui::layout::{Layout, Constraint, Direction};
use tui::widgets::TableState;
use unicode_width::UnicodeWidthStr;

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

            let prefix = if self.selected_tags.contains(tag) { "> " } else { "" };
            let prefix_w = UnicodeWidthStr::width(prefix);

            // Reserve one extra column between the tag text and the count so there
            // is always at least one space: [tag][space][count]
            let avail_for_tag = if max_width > count_w + 1 { max_width - count_w - 1 } else { 0 };
            let avail_for_tag = avail_for_tag.saturating_sub(prefix_w);

            let mut display_tag = String::new();
            let mut cur_w = 0usize;
            for ch in tag.chars() {
                let cw = UnicodeWidthStr::width(ch.to_string().as_str());
                if cur_w + cw > avail_for_tag { break; }
                display_tag.push(ch);
                cur_w += cw;
            }
            if display_tag.len() < tag.len() && avail_for_tag >= 3 {
                if cur_w + 3 <= avail_for_tag {
                    display_tag.push_str("...");
                }
            }

            let left_text = format!("{}{}", prefix, display_tag);

            let mut style = Style::default();
            let highlighted = if self.focus {
                if let Some(sel) = self.tags_selected { sel == i } else { false }
            } else { false };
            if highlighted { style = style.add_modifier(Modifier::REVERSED); }

            if highlighted {
                let fill = std::iter::repeat(' ').take(max_width).collect::<String>();
                buf.set_stringn(area.x, y, &fill, max_width, style);
            }

            buf.set_stringn(area.x, y, &left_text, max_width, style);
            // ensure a single space between tag text and count
            let x_count = area.x + (area.width as u16).saturating_sub(count_w as u16).saturating_sub(1);
            buf.set_stringn(x_count, y, " ", 1, style);
            let x_count = x_count + 1;
            buf.set_stringn(x_count, y, &count_str, count_str.len(), style);
            y += 1;
        }
    }
}

fn read_preview(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    let f = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(f);
    let mut buf = String::new();
    reader.take(64 * 1024).read_to_string(&mut buf)?;
    Ok(buf)
}

struct RawPreview<'a> { text: &'a str }
impl<'a> Widget for RawPreview<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut y = area.y as u16;
        let max_lines = area.height as usize;
        let max_width = area.width as usize;
        for (i, line) in self.text.lines().enumerate() {
            if i >= max_lines { break; }
            let line = line.replace('\t', "    ");
            let mut acc = String::new();
            let mut cur_w = 0usize;
            for ch in line.chars() {
                let cw = UnicodeWidthStr::width(ch.to_string().as_str());
                if cur_w + cw > max_width { break; }
                acc.push(ch);
                cur_w += cw;
            }
            let out = acc.replace(' ', "\u{00A0}");
            buf.set_stringn(area.x, y, &out, max_width, Style::default());
            y += 1;
        }
    }
}

fn human_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KIB { format!("{} B", bytes) }
    else if b < KIB.powi(2) { format!("{:.1} KiB", b / KIB) }
    else if b < KIB.powi(3) { format!("{:.1} MiB", b / KIB.powi(2)) }
    else if b < KIB.powi(4) { format!("{:.1} GiB", b / KIB.powi(3)) }
    else if b < KIB.powi(5) { format!("{:.1} TiB", b / KIB.powi(4)) }
    else { format!("{:.1} PiB", b / KIB.powi(5)) }
}

fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        if ai.peek().is_none() && bi.peek().is_none() { return Ordering::Equal; }
        if ai.peek().is_none() { return Ordering::Less; }
        if bi.peek().is_none() { return Ordering::Greater; }
        if ai.peek().unwrap().is_ascii_digit() && bi.peek().unwrap().is_ascii_digit() {
            let mut an = String::new(); while ai.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) { an.push(ai.next().unwrap()); }
            let mut bn = String::new(); while bi.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) { bn.push(bi.next().unwrap()); }
            let ai_num = an.trim_start_matches('0').parse::<u128>().ok().unwrap_or(0);
            let bi_num = bn.trim_start_matches('0').parse::<u128>().ok().unwrap_or(0);
            if ai_num != bi_num { return ai_num.cmp(&bi_num); }
            continue;
        }
        let ac = ai.next().unwrap(); let bc = bi.next().unwrap();
        let acu = ac.to_ascii_lowercase(); let bcu = bc.to_ascii_lowercase();
        if acu != bcu { return acu.cmp(&bcu); }
    }
}

fn filter_indices_mode(selected_tags: &HashSet<String>, tags_vec: &Vec<Vec<String>>, match_all: bool) -> Vec<usize> {
    if selected_tags.is_empty() { return (0..tags_vec.len()).collect(); }
    let need: Vec<&String> = selected_tags.iter().collect();
    let mut out: Vec<usize> = Vec::new();
    for (i, tv) in tags_vec.iter().enumerate() {
        if match_all {
            let mut ok = true; for t in need.iter() { if !tv.iter().any(|x| x == *t) { ok = false; break; } }
            if ok { out.push(i); }
        } else {
            let mut ok = false; for t in need.iter() { if tv.iter().any(|x| x == *t) { ok = true; break; } }
            if ok { out.push(i); }
        }
    }
    out
}

fn scan_dumps(dumps_dir: &std::path::Path) -> (Vec<(String, String, String)>, Vec<std::path::PathBuf>, Vec<Vec<String>>) {
    let mut files: Vec<(std::path::PathBuf, SystemTime, std::fs::Metadata, String, Vec<String>, Option<i64>)> = Vec::new();
    if let Ok(rd) = fs::read_dir(dumps_dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let path = e.path(); if !path.is_file() { continue; }
            if let Some(fname) = path.file_name().and_then(|s| s.to_str()) {
                if !fname.ends_with(".json") || fname.ends_with(".metadata.json") { continue; }
            }
            if let Ok(meta) = e.metadata() {
                let file_mtime = meta.modified().ok().unwrap_or(SystemTime::UNIX_EPOCH);
                let (title, tags, meta_ts) = read_metadata_for_path(&path, &dumps_dir);
                // meta_ts is now in milliseconds when present. Build effective using millis precision.
                let effective = if let Some(sts) = meta_ts {
                    if sts >= 0 {
                        SystemTime::UNIX_EPOCH + Duration::from_millis(sts as u64)
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
    files.sort_by_key(|(_, eff, ..)| *eff); files.reverse();
    let mut entries: Vec<(String, String, String)> = Vec::new();
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    let mut tags_vec: Vec<Vec<String>> = Vec::new();
    for (path, effective, meta, title, tags, meta_ts) in files.into_iter() {
        // meta_ts is milliseconds. Use timestamp_millis for display when available.
        let ts_string = if let Some(sts) = meta_ts {
            if let Some(dt) = Local.timestamp_millis_opt(sts).single() {
                dt.format("%Y-%m-%d %H:%M:%S").to_string()
            } else {
                DateTime::<Local>::from(effective).format("%Y-%m-%d %H:%M:%S").to_string()
            }
        } else {
            DateTime::<Local>::from(effective).format("%Y-%m-%d %H:%M:%S").to_string()
        };
        let size_str = human_size(meta.len()); entries.push((ts_string, title, size_str)); paths.push(path.clone()); tags_vec.push(tags);
    }
    (entries, paths, tags_vec)
}

fn get_mtime(path: &std::path::Path) -> Option<SystemTime> { path.metadata().and_then(|m| m.modified()).ok() }

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus { Tags, Dumps }

fn build_entry_from_path(path: &std::path::Path) -> Option<((String, String, String), SystemTime)> {
    if !path.is_file() { return None; }
    if let Some(fname) = path.file_name().and_then(|s| s.to_str()) { if fname.ends_with(".metadata.json") { return None; } }
    let meta = path.metadata().ok()?; let mtime = meta.modified().ok()?; let size = meta.len();
    let fname = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ts = DateTime::<Local>::from(mtime).format("%Y-%m-%d %H:%M:%S").to_string();
    let mut base = fname.clone(); if base.ends_with(".json") { base.truncate(base.len() - 5); }
    let title = if let Some(idx) = base.rfind('_') { let t = &base[..idx]; if t.is_empty() { "".to_string() } else { t.to_string() } } else { "".to_string() };
    let size_str = human_size(size); Some(((ts, title, size_str), mtime))
}

fn read_metadata_for_path(path: &std::path::Path, dumps_dir: &std::path::Path) -> (String, Vec<String>, Option<i64>) {
    let mut title = String::new(); let mut tags: Vec<String> = Vec::new(); let mut timestamp: Option<i64> = None;
    if let Some(fname) = path.file_name().and_then(|s| s.to_str()) {
        if fname.ends_with(".json") {
            let id = &fname[..fname.len() - 5];
            let meta_path = dumps_dir.join(format!("{}.metadata.json", id));
            if let Ok(s) = std::fs::read_to_string(&meta_path) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                    if let Some(t) = v.get("title").and_then(|x| x.as_str()) { title = t.to_string(); }
                    if let Some(arr) = v.get("tags").and_then(|x| x.as_array()) { for it in arr.iter() { if let Some(tsv) = it.as_str() { tags.push(tsv.to_string()); } } }
                    if let Some(tv) = v.get("timestamp") {
                        // accept number or numeric string; normalize to milliseconds
                        if let Some(n) = tv.as_i64() { timestamp = Some(n); }
                        else if let Some(un) = tv.as_u64() { timestamp = Some(un as i64); }
                        else if let Some(sv) = tv.as_str() { if let Ok(parsed) = sv.parse::<i64>() { timestamp = Some(parsed); } }
                        if let Some(tsv) = timestamp {
                            // If value looks like milliseconds (large value) keep as-is.
                            // If it looks like seconds (small value) convert to milliseconds.
                            if tsv.abs() <= 3_000_000_000i64 {
                                // treat as seconds -> convert to milliseconds
                                timestamp = Some(tsv * 1000);
                            } else {
                                // already milliseconds; leave as-is
                            }
                        }
                    }
                }
            }
        }
    }
    (title, tags, timestamp)
}

fn apply_watch_event(ev: WatchEvent, dumps_dir: &std::path::Path, entries: &mut Vec<(String, String, String)>, paths: &mut Vec<std::path::PathBuf>, tags_vec: &mut Vec<Vec<String>>, selected_tags: &mut HashSet<String>, state: &mut TableState, preview: &mut String, match_all: bool) {
    match ev {
        WatchEvent::Rescan => {
            let (new_entries, new_paths, new_tags) = scan_dumps(dumps_dir);
            *entries = new_entries; *paths = new_paths; *tags_vec = new_tags.clone();
            let mut all: HashSet<String> = HashSet::new(); for t in tags_vec.iter().flat_map(|v| v.iter()) { all.insert(t.clone()); }
            selected_tags.retain(|t| all.contains(t));
            if entries.is_empty() { *preview = String::new(); state.select(None); } else {
                match state.selected() {
                    Some(i) if i < entries.len() => { if let Some(p) = paths.get(i) { *preview = read_preview(p).unwrap_or_else(|_e| String::new()); } }
                    _ => { state.select(Some(0)); if let Some(p) = paths.get(0) { *preview = read_preview(p).unwrap_or_else(|_e| String::new()); } }
                }
            }
        }
        WatchEvent::Created(p) => {
            if !p.starts_with(dumps_dir) { return; }
            if let Some((entry, mtime)) = build_entry_from_path(&p) {
                let (title_meta, tags_meta, meta_ts) = read_metadata_for_path(&p, dumps_dir);
                let mut real_entry = entry.clone(); if !title_meta.is_empty() { real_entry.1 = title_meta; }
                let mut use_mtime = mtime;
                if let Some(sts) = meta_ts { if let Some(dt) = Local.timestamp_opt(sts, 0).single() { real_entry.0 = dt.format("%Y-%m-%d %H:%M:%S").to_string(); } if sts >= 0 { use_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(sts as u64); } }
                if let Some(pos) = paths.iter().position(|x| x == &p) {
                    entries[pos] = real_entry.clone(); tags_vec[pos] = tags_meta; if state.selected() == Some(pos) { *preview = read_preview(&p).unwrap_or_else(|_e| String::new()); }
                } else {
                    let mut inserted = false;
                    for (i, existing) in paths.iter().enumerate() {
                        let existing_meta_ts = read_metadata_for_path(existing, dumps_dir).2;
                        let existing_effective = if let Some(est) = existing_meta_ts {
                            if est >= 0 { SystemTime::UNIX_EPOCH + Duration::from_millis(est as u64) } else { get_mtime(existing).unwrap_or(SystemTime::UNIX_EPOCH) }
                        } else { get_mtime(existing).unwrap_or(SystemTime::UNIX_EPOCH) };
                        if use_mtime > existing_effective { paths.insert(i, p.clone()); entries.insert(i, real_entry.clone()); tags_vec.insert(i, tags_meta.clone()); if let Some(sel) = state.selected() { if i <= sel { state.select(Some(sel + 1)); } } inserted = true; break; }
                    }
                    if !inserted { paths.push(p.clone()); entries.push(real_entry); tags_vec.push(tags_meta.clone()); }
                    // Only jump to the newest dump if it is visible under the current tag
                    // filter. Compute visibility from the inserted file's tags and the
                    // selected_tags/match_all mode.
                    let visible = if selected_tags.is_empty() {
                        true
                    } else if match_all {
                        // match all: every selected tag must be present on the dump
                        selected_tags.iter().all(|t| tags_meta.iter().any(|x| x == t))
                    } else {
                        // match any: any selected tag present on the dump
                        selected_tags.iter().any(|t| tags_meta.iter().any(|x| x == t))
                    };
                    if visible {
                        state.select(Some(0));
                        if let Some(first) = paths.get(0) { *preview = read_preview(first).unwrap_or_else(|_e| String::new()); }
                    }
                }
            } else {
                if let Some(pos) = paths.iter().position(|x| x == &p) { paths.remove(pos); entries.remove(pos); tags_vec.remove(pos); match state.selected() { Some(sel) if sel == pos => { if entries.is_empty() { state.select(None); *preview = String::new(); } else { let new_sel = if pos == 0 { 0 } else { pos - 1 }; state.select(Some(new_sel)); if let Some(p2) = paths.get(new_sel) { *preview = read_preview(p2).unwrap_or_else(|_e| String::new()); } } } Some(sel) if sel > pos => { state.select(Some(sel - 1)); } _ => {} } }
            }
        }
        WatchEvent::Modified(p) => {
            if !p.starts_with(dumps_dir) { return; }
            if let Some((entry, mtime)) = build_entry_from_path(&p) {
                let (title_meta, tags_meta, meta_ts) = read_metadata_for_path(&p, dumps_dir);
                let mut real_entry = entry.clone(); if !title_meta.is_empty() { real_entry.1 = title_meta; }
                let mut use_mtime = mtime;
                if let Some(sts) = meta_ts { if let Some(dt) = Local.timestamp_opt(sts, 0).single() { real_entry.0 = dt.format("%Y-%m-%d %H:%M:%S").to_string(); } if sts >= 0 { use_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(sts as u64); } }
                if let Some(pos) = paths.iter().position(|x| x == &p) {
                    entries[pos] = real_entry.clone(); tags_vec[pos] = tags_meta; if state.selected() == Some(pos) { *preview = read_preview(&p).unwrap_or_else(|_e| String::new()); }
                } else {
                    let mut inserted = false;
                    for (i, existing) in paths.iter().enumerate() {
                        let existing_meta_ts = read_metadata_for_path(existing, dumps_dir).2;
                        let existing_effective = if let Some(est) = existing_meta_ts {
                            if est >= 0 { SystemTime::UNIX_EPOCH + Duration::from_millis(est as u64) } else { get_mtime(existing).unwrap_or(SystemTime::UNIX_EPOCH) }
                        } else { get_mtime(existing).unwrap_or(SystemTime::UNIX_EPOCH) };
                        if use_mtime > existing_effective { paths.insert(i, p.clone()); entries.insert(i, real_entry.clone()); tags_vec.insert(i, tags_meta.clone()); if let Some(sel) = state.selected() { if i <= sel { state.select(Some(sel + 1)); } } inserted = true; break; }
                    }
                    if !inserted { paths.push(p.clone()); entries.push(real_entry); tags_vec.push(tags_meta.clone()); }
                    let visible = if selected_tags.is_empty() {
                        true
                    } else if match_all {
                        selected_tags.iter().all(|t| tags_meta.iter().any(|x| x == t))
                    } else {
                        selected_tags.iter().any(|t| tags_meta.iter().any(|x| x == t))
                    };
                    if visible {
                        state.select(Some(0));
                        if let Some(first) = paths.get(0) { *preview = read_preview(first).unwrap_or_else(|_e| String::new()); }
                    }
                }
            } else {
                if let Some(pos) = paths.iter().position(|x| x == &p) { paths.remove(pos); entries.remove(pos); tags_vec.remove(pos); match state.selected() { Some(sel) if sel == pos => { if entries.is_empty() { state.select(None); *preview = String::new(); } else { let new_sel = if pos == 0 { 0 } else { pos - 1 }; state.select(Some(new_sel)); if let Some(p2) = paths.get(new_sel) { *preview = read_preview(p2).unwrap_or_else(|_e| String::new()); } } } Some(sel) if sel > pos => { state.select(Some(sel - 1)); } _ => {} } }
            }
        }
        WatchEvent::Removed(p) => { if let Some(pos) = paths.iter().position(|x| x == &p) { paths.remove(pos); entries.remove(pos); tags_vec.remove(pos); match state.selected() { Some(sel) if sel == pos => { if entries.is_empty() { state.select(None); *preview = String::new(); } else { let new_sel = if pos == 0 { 0 } else { pos - 1 }; state.select(Some(new_sel)); if let Some(p2) = paths.get(new_sel) { *preview = read_preview(p2).unwrap_or_else(|_e| String::new()); } } } Some(sel) if sel > pos => { state.select(Some(sel - 1)); } _ => {} } } }
    }
}

#[derive(Debug)]
enum WatchEvent { Created(std::path::PathBuf), Modified(std::path::PathBuf), Removed(std::path::PathBuf), Rescan }

pub fn run_tui() -> Result<(), Box<dyn std::error::Error>> {
    // Determine dumps directory
    let mut dumps_dir: PathBuf = match env::var("XDG_DATA_HOME") { Ok(x) if !x.is_empty() => PathBuf::from(x).join("rubbish").join("dumps"), _ => match env::var("HOME") { Ok(h) => PathBuf::from(h).join(".local").join("share").join("rubbish").join("dumps"), Err(_) => PathBuf::from("./dumps"), }, };
    if let Err(_e) = fs::create_dir_all(&dumps_dir) { dumps_dir = PathBuf::from("./dumps"); let _ = fs::create_dir_all(&dumps_dir); }

    let mut files: Vec<(std::path::PathBuf, Option<SystemTime>, u64)>;
    if let Ok(rd) = fs::read_dir(&dumps_dir) {
        files = rd.filter_map(|e| e.ok()).map(|e| { let path = e.path(); if !path.is_file() { return None; } if let Some(fname) = path.file_name().and_then(|s| s.to_str()) { if fname.ends_with(".metadata.json") { return None; } } let meta = path.metadata().ok(); let mtime = meta.as_ref().and_then(|m| m.modified().ok()); let size = meta.as_ref().map(|m| m.len()).unwrap_or(0); Some((path, mtime, size)) }).filter_map(|x| x).collect();
    } else { files = Vec::new(); }

    files.sort_by_key(|(_, mtime, _)| mtime.unwrap_or(SystemTime::UNIX_EPOCH)); files.reverse();
    let mut entries: Vec<(String, String, String)> = Vec::new(); let mut paths = Vec::new(); let mut tags_vec: Vec<Vec<String>> = Vec::new(); let mut selected_tags: HashSet<String> = HashSet::new();
    for (path, mtime, size) in files.iter() { let fname = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(); let ts = mtime.as_ref().map(|t| DateTime::<Local>::from(*t).format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_else(|| "unknown".into()); let (meta_title, _meta_tags, _meta_ts) = read_metadata_for_path(path, &dumps_dir); let title = if !meta_title.is_empty() { meta_title } else { let mut base = fname.clone(); if base.ends_with(".json") { base.truncate(base.len() - 5);} if let Some(idx) = base.rfind('_') { let t = &base[..idx]; if t.is_empty() { "".to_string() } else { t.to_string() } } else { "".to_string() } }; let size_str = human_size(*size); entries.push((ts, title, size_str)); paths.push(path.clone()); let (_t, tg, _meta_ts) = read_metadata_for_path(path, &dumps_dir); tags_vec.push(tg); }

    let mut preview = String::new(); if let Some(p) = paths.get(0) { preview = read_preview(p).unwrap_or_else(|_e| String::new()); }

    let (fs_tx, fs_rx) = std::sync::mpsc::channel::<WatchEvent>(); let watch_dir = dumps_dir.clone();
    {
        let tx = fs_tx.clone(); let watch_dir2 = watch_dir.clone();
        thread::spawn(move || {
            use notify::{Watcher, RecursiveMode, RecommendedWatcher, EventKind}; use std::sync::mpsc::RecvTimeoutError;
            if let Err(_e) = (|| -> Result<(), Box<dyn std::error::Error>> {
                let (local_tx, rx) = std::sync::mpsc::channel();
                let mut watcher: RecommendedWatcher = RecommendedWatcher::new(local_tx, notify::Config::default())?;
                watcher.watch(&watch_dir2, RecursiveMode::NonRecursive)?;
                loop { match rx.recv_timeout(Duration::from_secs(1)) { Ok(Ok(ev)) => { for p in ev.paths.iter() { let we = match &ev.kind { EventKind::Create(_) => WatchEvent::Created(p.clone()), EventKind::Modify(_) => WatchEvent::Modified(p.clone()), EventKind::Remove(_) => WatchEvent::Removed(p.clone()), _ => WatchEvent::Rescan, }; let _ = tx.send(we); } } Ok(Err(_)) => { let _ = tx.send(WatchEvent::Rescan); }, Err(RecvTimeoutError::Timeout) => continue, Err(_) => break, } }
                Ok(())
            })() {
                // fallback polling
                let tx2 = tx.clone(); let watch_dir3 = watch_dir2.clone();
                std::thread::spawn(move || {
                    let mut last: HashMap<String, std::time::SystemTime> = HashMap::new();
                    loop {
                        let mut current: HashMap<String, std::time::SystemTime> = HashMap::new();
                        if let Ok(rd) = std::fs::read_dir(&watch_dir3) { for e in rd.filter_map(|e| e.ok()) { let p = e.path(); if p.is_file() { if let Ok(m) = e.metadata().and_then(|m| m.modified()) { current.insert(p.to_string_lossy().to_string(), m); } } } }
                        if current != last {
                            for (k, m) in current.iter() {
                                if !last.contains_key(k) { let _ = tx2.send(WatchEvent::Created(std::path::PathBuf::from(k))); } else if last.get(k).map(|t| t != m).unwrap_or(false) { let _ = tx2.send(WatchEvent::Modified(std::path::PathBuf::from(k))); }
                            }
                            for k in last.keys() { if !current.contains_key(k) { let _ = tx2.send(WatchEvent::Removed(std::path::PathBuf::from(k))); } }
                            last = current;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                    }
                });
            }
        });
    }

    // Setup terminal and interactive UI
    enable_raw_mode()?; let mut stdout = io::stdout(); execute!(stdout, EnterAlternateScreen)?; let backend = CrosstermBackend::new(stdout); let mut terminal = Terminal::new(backend)?;

    let mut state = TableState::default(); if entries.is_empty() { state.select(None); } else { state.select(Some(0)); }
    let mut tags_selected: Option<usize> = None; let mut focus: Focus = Focus::Dumps; let mut match_all: bool = true;

    let pager_cmd: Option<String> = if Command::new("jless").arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false) { Some("jless".to_string()) } else if Command::new("less").arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false) { Some("less".to_string()) } else { None };

    let (sig_tx, sig_rx) = channel(); let mut signals = Signals::new(&[SIGINT, SIGTERM, SIGQUIT]).unwrap(); thread::spawn(move || { for sig in signals.forever() { let _ = sig_tx.send(sig); } });

    loop {
        if let Ok(ev) = fs_rx.try_recv() { apply_watch_event(ev, &dumps_dir, &mut entries, &mut paths, &mut tags_vec, &mut selected_tags, &mut state, &mut preview, match_all); }
        let mut uniq_set: HashSet<String> = HashSet::new(); for v in tags_vec.iter() { for t in v.iter() { uniq_set.insert(t.clone()); } }
        let mut unique_tags: Vec<String> = uniq_set.into_iter().collect(); unique_tags.sort_by(|a, b| natural_cmp(a, b));
        if unique_tags.is_empty() { tags_selected = None; } else if tags_selected.is_none() { tags_selected = Some(0); } else if let Some(idx) = tags_selected { if idx >= unique_tags.len() { tags_selected = Some(0); } }

        let display_indices: Vec<usize> = filter_indices_mode(&selected_tags, &tags_vec, match_all);
        terminal.draw(|f| {
            let size = f.size(); let vchunks = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(size.height.saturating_sub(1)), Constraint::Length(1)].as_ref()).split(size);
            // Increase the main content area by 1 row so boxes include the bottom line
            let mut content_area = vchunks[0];
            content_area.height = content_area.height.saturating_add(1);

            // If there are no tags, reserve a small fixed-width column for the
            // Dumps box (keep its width unchanged) and expand the preview to the
            // right. When tags are present use the normal percentage split.
            // Build horizontal columns. We always keep the Dumps box at a
            // static width (23) so it never resizes when the terminal changes
            // size. When tags exist we allocate three columns: Tags (percent),
            // Dumps (fixed Length(23)), Preview (rest). When tags are absent we
            // allocate two columns: Dumps (fixed) and Preview (rest).
            let (chunks, left_chunks) = if unique_tags.is_empty() {
                let chunks = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Length(23), Constraint::Min(0)].as_ref()).split(content_area);
                // left_chunks: [tags-area(empty), dumps-area]
                let left_chunks = vec![Rect { x: chunks[0].x, y: chunks[0].y, width: 0, height: chunks[0].height }, chunks[0]];
                (chunks, left_chunks)
            } else {
                // Three columns: Tags | Dumps(fixed) | Preview
                let mut chunks = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(40), Constraint::Length(23), Constraint::Min(0)].as_ref()).split(content_area);
                // transfer 10 columns from Tags to Preview to make Tags narrower
                let transfer: u16 = 10;
                if chunks[0].width > transfer {
                    chunks[0].width = chunks[0].width.saturating_sub(transfer);
                    chunks[2].x = chunks[2].x.saturating_sub(transfer);
                    chunks[2].width = chunks[2].width.saturating_add(transfer);
                }
                // left_chunks: [tags-area, dumps-area]
                let left_chunks = vec![chunks[0], chunks[1]];
                (chunks, left_chunks)
            };

            // Derive explicit tags and dumps rects so the Dumps area remains a
            // fixed-size Rect (23 columns outer) regardless of resizing.
            let tags_area: Rect = if unique_tags.is_empty() { Rect { x: chunks[0].x, y: chunks[0].y, width: 0, height: chunks[0].height } } else { chunks[0] };
            let dumps_area: Rect = if unique_tags.is_empty() { chunks[0] } else { chunks[1] };

            let rows: Vec<Row> = display_indices.iter().filter_map(|&i| entries.get(i).map(|e| (i, e.clone()))).map(|(i, (ts, _title, _size_str))| { let prefix = if Some(i) == state.selected() { "  " } else { "" }; let cell = format!("{}{}", prefix, ts); Row::new(vec![Cell::from(cell)]) }).collect();

            let table_block = Block::default().borders(Borders::ALL).title("Dumps"); let table = Table::new(rows).block(table_block.clone()).widths(&[Constraint::Length(21)]).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
            let total = entries.len(); let shown = display_indices.len();

            if entries.is_empty() { let empty_block = Block::default().borders(Borders::ALL).title("Dumps"); f.render_widget(empty_block, dumps_area); let counts = format!("{}/{}", shown, total); let count_w = UnicodeWidthStr::width(counts.as_str()) as u16; let count_area = Rect { x: dumps_area.x + dumps_area.width.saturating_sub(count_w + 1), y: dumps_area.y, width: count_w, height: 1 }; if !unique_tags.is_empty() { f.render_widget(Paragraph::new(counts), count_area); } }
            else if display_indices.is_empty() { let empty_block = Block::default().borders(Borders::ALL).title("Dumps"); f.render_widget(empty_block, dumps_area); let counts = format!("{}/{}", shown, total); let count_w = UnicodeWidthStr::width(counts.as_str()) as u16; let count_area = Rect { x: dumps_area.x + dumps_area.width.saturating_sub(count_w + 1), y: dumps_area.y, width: count_w, height: 1 }; if !unique_tags.is_empty() { f.render_widget(Paragraph::new(counts), count_area); } }
            else {
                let mut display_state = tui::widgets::TableState::default(); if let Some(master_sel) = state.selected() { if let Some(pos) = display_indices.iter().position(|&x| x == master_sel) { display_state.select(Some(pos)); } else { display_state.select(None); } } else { display_state.select(None); }
                if focus == Focus::Dumps { f.render_stateful_widget(table, dumps_area, &mut display_state); } else { f.render_widget(table, dumps_area); }
                let counts = format!("{}/{}", shown, total); let count_w = UnicodeWidthStr::width(counts.as_str()) as u16; let count_area = Rect { x: dumps_area.x + dumps_area.width.saturating_sub(count_w + 1), y: dumps_area.y, width: count_w, height: 1 }; if !unique_tags.is_empty() { f.render_widget(Paragraph::new(counts), count_area); }
                if let Some(master_sel) = state.selected() { if let Some(display_pos) = display_indices.iter().position(|&x| x == master_sel) { let inner = table_block.inner(dumps_area); struct Marker; impl Widget for Marker { fn render(self, area: Rect, buf: &mut Buffer) { let y = area.y as u16; buf.set_stringn(area.x, y, ">", 1, Style::default().add_modifier(Modifier::BOLD)); } } if display_pos < inner.height as usize { let mut area = inner; area.y = inner.y + display_pos as u16; area.height = 1; f.render_widget(Marker, area); } } }
            }

            // Determine block title from the dump's metadata `title` (rubbish-title).
            // If no rubbish-title was supplied, render an empty title.
            let preview_title: String = match state.selected().and_then(|i| paths.get(i)) {
                Some(pth) => {
                    let (meta_title, _meta_tags, _meta_ts) = read_metadata_for_path(pth, &dumps_dir);
                    if meta_title.is_empty() { String::new() } else { meta_title }
                }
                None => String::new(),
            };
            let preview_widget = RawPreview { text: &preview };
            let block = Block::default().borders(Borders::ALL).title(preview_title);
            let inner = block.inner(chunks[1]);
            f.render_widget(block, chunks[1]);
            f.render_widget(preview_widget, inner);

            // Only render the Tags box when there are tags to show. If there are no
            // tags, don't render anything in the left tag column so the UI is not
            // cluttered with an empty box.
            if !unique_tags.is_empty() {
                let mut counts: Vec<usize> = Vec::with_capacity(unique_tags.len());
                for t in unique_tags.iter() { let cnt = tags_vec.iter().filter(|tv| tv.iter().any(|x| x == t)).count(); counts.push(cnt); }
                let tags_block = Block::default().borders(Borders::ALL).title("Tags");
                let tags_block_clone = tags_block.clone();
                f.render_widget(tags_block_clone, tags_area);
                let inner = tags_block.inner(tags_area);
                let raw = RawTags { tags: &unique_tags, counts: &counts, selected_tags: &selected_tags, focus: focus == Focus::Tags, tags_selected };
                f.render_widget(raw, inner);
            }

            // Render the selected dump's size in the bottom-right of the preview box
            let sel_size = match state.selected().and_then(|i| entries.get(i)) {
                Some((_ts, _title, size_str)) => size_str.clone(),
                None => String::new(),
            };
            if !sel_size.is_empty() {
                let mut size_display = sel_size.clone();
                // render the size inline with the preview block's bottom border
                let outer = chunks[1];
                let outer_w = outer.width as usize;
                if outer_w > 0 {
                    let mut size_w = UnicodeWidthStr::width(size_display.as_str());
                    if size_w + 1 >= outer_w {
                        // truncate to fit into the outer width (reserve 1 for border spacing)
                        let mut acc = String::new();
                        let mut cur_w = 0usize;
                        // allow at most outer_w - 1 characters worth of width
                        let max_w = outer_w.saturating_sub(1);
                        for ch in size_display.chars() {
                            let cw = UnicodeWidthStr::width(ch.to_string().as_str());
                            if cur_w + cw > max_w { break; }
                            acc.push(ch);
                            cur_w += cw;
                        }
                        size_display = acc;
                        size_w = UnicodeWidthStr::width(size_display.as_str());
                    }
                    let x = outer.x + outer.width.saturating_sub(size_w as u16 + 1);
                    let y = outer.y + outer.height.saturating_sub(1);
                    let area = Rect { x, y, width: size_w as u16, height: 1 };
                    f.render_widget(Paragraph::new(size_display), area);
                }
            }
        })?;

        if event::poll(Duration::from_millis(200))? {
            if let CEvent::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Left | KeyCode::Char('h') => {
                        focus = Focus::Tags;
                        if let Some(dsel) = state.selected() {
                            if let Some(first_tag) = tags_vec.get(dsel).and_then(|v| v.get(0)) {
                                tags_selected = unique_tags.iter().position(|t| t == first_tag);
                                if tags_selected.is_none() && !unique_tags.is_empty() {
                                    tags_selected = Some(0);
                                }
                            } else {
                                tags_selected = None;
                            }
                        } else {
                            tags_selected = None;
                        }
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        focus = Focus::Dumps;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        match focus {
                            Focus::Dumps => {
                                if entries.is_empty() {
                                    // nothing
                                } else if display_indices.is_empty() {
                                    // no visible items
                                } else {
                                    let cur_pos = state.selected().and_then(|ms| display_indices.iter().position(|&x| x == ms));
                                    let new_pos = match cur_pos {
                                        Some(0) | None => display_indices.len() - 1,
                                        Some(p) => p - 1,
                                    };
                                    let master_idx = display_indices[new_pos];
                                    state.select(Some(master_idx));
                                    if let Some(p) = paths.get(master_idx) {
                                        preview = read_preview(p).unwrap_or_else(|_e| String::new());
                                    }
                                    tags_selected = tags_vec.get(master_idx).and_then(|v| if v.is_empty() { None } else { Some(0) });
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
                    KeyCode::Down | KeyCode::Char('j') => {
                        match focus {
                            Focus::Dumps => {
                                if entries.is_empty() {
                                    // nothing
                                } else if display_indices.is_empty() {
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
                                        preview = read_preview(p).unwrap_or_else(|_e| String::new());
                                    }
                                    tags_selected = tags_vec.get(master_idx).and_then(|v| if v.is_empty() { None } else { Some(0) });
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
                        if focus == Focus::Tags {
                            if let Some(tsel) = tags_selected {
                                if let Some(tag) = unique_tags.get(tsel) {
                                    if selected_tags.contains(tag) { selected_tags.remove(tag); } else { selected_tags.insert(tag.clone()); }
                                    // after changing the active tag filter, ensure visibility/selection
                                    if entries.is_empty() {
                                        // nothing
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
                                            preview = String::new();
                                        } else {
                                            match state.selected() {
                                                Some(ms) if new_display.iter().any(|&x| x == ms) => {}
                                                _ => {
                                                    state.select(Some(new_display[0]));
                                                    if let Some(p) = paths.get(new_display[0]) {
                                                        preview = read_preview(p).unwrap_or_else(|_e| String::new());
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
                        selected_tags.clear();
                        if !entries.is_empty() {
                            state.select(Some(0));
                            if let Some(p) = paths.get(0) {
                                preview = read_preview(p).unwrap_or_else(|_e| String::new());
                            }
                        } else {
                            state.select(None);
                            preview = String::new();
                        }
                    }
                    KeyCode::Char('m') => {
                        match_all = !match_all;
                        let new_display = filter_indices_mode(&selected_tags, &tags_vec, match_all);
                        if new_display.is_empty() {
                            state.select(None);
                            preview = String::new();
                        } else {
                            match state.selected() {
                                Some(ms) if new_display.iter().any(|&x| x == ms) => {}
                                _ => {
                                    state.select(Some(new_display[0]));
                                    if let Some(p) = paths.get(new_display[0]) {
                                        preview = read_preview(p).unwrap_or_else(|_e| String::new());
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
                                preview = read_preview(&path).unwrap_or_else(|_e| String::new());

                                // Small delay then redraw
                                std::thread::sleep(Duration::from_millis(100));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if sig_rx.try_recv().is_ok() { break; }
    }

    disable_raw_mode()?; execute!(terminal.backend_mut(), LeaveAlternateScreen)?; terminal.show_cursor()?;

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
        let dir = tempdir().expect("tempdir");
        let mut path = dir.path().to_path_buf();
        let fname = format!("2026-03-18_test-title-{}_.json", uuid::Uuid::new_v4());
        path.push(&fname);

        let data = b"{\"x\":1}\n";
        {
            let mut f = fs::File::create(&path).expect("create temp file");
            f.write_all(data).expect("write data");
        }

        std::thread::sleep(std::time::Duration::from_millis(10));

        let res = build_entry_from_path(&path).expect("should build entry");
        let ((ts, title, size_str), _mtime) = res;

        assert!(title.contains("test-title"));
        assert!(size_str.ends_with("B") || size_str.contains("KiB"));
        assert!(!ts.is_empty());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_scan_dumps_ignores_metadata_and_reads_titles() {
        let dir = tempdir().expect("tempdir");
        let dumps_dir = dir.path();

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
        tags_vec.push(vec!["a".to_string(), "b".to_string()]);
        tags_vec.push(vec!["b".to_string(), "c".to_string()]);
        tags_vec.push(vec!["a".to_string(), "c".to_string()]);

        let mut sel: HashSet<String> = HashSet::new();
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
        tags_vec.push(vec!["a".to_string(), "b".to_string()]);
        tags_vec.push(vec!["b".to_string(), "c".to_string()]);
        tags_vec.push(vec!["d".to_string()]);

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

        let id = "ts_test_id";
        let dump_path = dumps_dir.join(format!("{}.json", id));
        fs::write(&dump_path, b"{}\n").expect("write dump");

        let ts_secs: i64 = 1_700_000_000;
        let meta_secs = serde_json::json!({"title": "S", "tags": ["t"], "timestamp": ts_secs});
        let meta_path = dumps_dir.join(format!("{}.metadata.json", id));
        fs::write(&meta_path, serde_json::to_string(&meta_secs).unwrap()).expect("write meta secs");

        let (_title, _tags, parsed_secs) = read_metadata_for_path(&dump_path, &dumps_dir);
        assert_eq!(parsed_secs, Some(ts_secs * 1000));

        let ts_millis: i64 = ts_secs * 1000;
        let meta_millis = serde_json::json!({"title": "M", "tags": ["t"], "timestamp": ts_millis});
        fs::write(&meta_path, serde_json::to_string(&meta_millis).unwrap()).expect("write meta millis");

        let (_title2, _tags2, parsed_millis) = read_metadata_for_path(&dump_path, &dumps_dir);
        assert_eq!(parsed_millis, Some(ts_millis));

        let meta_millis_str = serde_json::json!({"title":"MS","tags":["t"],"timestamp": ts_millis.to_string()});
        fs::write(&meta_path, serde_json::to_string(&meta_millis_str).unwrap()).expect("write meta millis str");

        let (_title3, _tags3, parsed_millis_str) = read_metadata_for_path(&dump_path, &dumps_dir);
        assert_eq!(parsed_millis_str, Some(ts_millis));
    }
}
