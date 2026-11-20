//! Discovers Claude Code and Codex chat sessions on disk.
//!
//! Session logs can be hundreds of megabytes, so only a bounded head and tail
//! of each file is read, files are parsed in parallel, and results are cached
//! by `(path, mtime, size)` so a rescan is a directory walk plus a few stats.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::links::home_dir;

const HEAD_BYTES: u64 = 192 * 1024;
const TAIL_BYTES: u64 = 128 * 1024;
const CODEX_HEAD_BYTES: u64 = 768 * 1024;
const CACHE_VERSION: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub agent: Agent,
    pub id: String,
    pub title: String,
    pub cwd: String,
    /// Seconds since the Unix epoch.
    pub modified: u64,
    pub path: PathBuf,
    /// Lowercased text matched by the search box.
    #[serde(skip)]
    pub haystack: String,
}

impl Session {
    /// The shell command that reopens this conversation.
    pub fn resume_command(&self) -> String {
        resume_command(self.agent, &self.id)
    }

    pub fn project_name(&self) -> &str {
        self.cwd
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("/")
    }

    fn finish(mut self) -> Self {
        self.haystack = format!(
            "{} {} {}",
            self.title.to_lowercase(),
            self.cwd.to_lowercase(),
            self.agent.label().to_lowercase()
        );
        self
    }
}

/// The shell command that reopens a conversation.
pub fn resume_command(agent: Agent, id: &str) -> String {
    match agent {
        Agent::Claude => format!("claude --resume {}", shell_quote(id)),
        Agent::Codex => format!("codex resume {}", shell_quote(id)),
    }
}

/// Quotes `text` for the shell terminals start.
pub fn shell_quote(text: &str) -> String {
    crate::terminal::shell::quote_for_default_shell(text)
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    path: PathBuf,
    mtime: u64,
    size: u64,
    /// `None` records a file that holds no listable conversation.
    session: Option<Session>,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    entries: Vec<CacheEntry>,
}

struct Candidate {
    agent: Agent,
    path: PathBuf,
    mtime: u64,
    size: u64,
}

fn cache_path() -> Option<PathBuf> {
    Some(crate::platform::cache_dir()?.join("sessions.json"))
}

fn load_cache() -> HashMap<PathBuf, CacheEntry> {
    let Some(path) = cache_path() else {
        return HashMap::new();
    };
    let Ok(bytes) = fs::read(path) else {
        return HashMap::new();
    };
    match serde_json::from_slice::<CacheFile>(&bytes) {
        Ok(cache) if cache.version == CACHE_VERSION => cache
            .entries
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect(),
        _ => HashMap::new(),
    }
}

fn save_cache(entries: &[CacheEntry]) {
    let Some(path) = cache_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    #[derive(Serialize)]
    struct CacheRef<'a> {
        version: u32,
        entries: &'a [CacheEntry],
    }
    if let Ok(bytes) = serde_json::to_vec(&CacheRef {
        version: CACHE_VERSION,
        entries,
    }) {
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, bytes).is_ok() {
            let _ = fs::rename(tmp, path);
        }
    }
}

fn file_stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((mtime, meta.len()))
}

fn collect_candidates(home: &Path) -> Vec<Candidate> {
    let mut out = Vec::new();

    // ~/.claude/projects/<encoded cwd>/<session id>.jsonl
    if let Ok(projects) = fs::read_dir(home.join(".claude/projects")) {
        for project in projects.flatten() {
            let Ok(files) = fs::read_dir(project.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().is_some_and(|ext| ext == "jsonl") {
                    if let Some((mtime, size)) = file_stamp(&path) {
                        out.push(Candidate {
                            agent: Agent::Claude,
                            path,
                            mtime,
                            size,
                        });
                    }
                }
            }
        }
    }

    // ~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl
    fn walk(dir: &Path, depth: usize, out: &mut Vec<Candidate>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth < 4 {
                    walk(&path, depth + 1, out);
                }
            } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                if let Some((mtime, size)) = file_stamp(&path) {
                    out.push(Candidate {
                        agent: Agent::Codex,
                        path,
                        mtime,
                        size,
                    });
                }
            }
        }
    }
    walk(&home.join(".codex/sessions"), 0, &mut out);
    out
}

/// Codex keeps user-visible thread names in a side index.
fn load_codex_names(home: &Path) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let Ok(text) = fs::read_to_string(home.join(".codex/session_index.jsonl")) else {
        return names;
    };
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let (Some(id), Some(name)) = (
            value.get("id").and_then(Value::as_str),
            value.get("thread_name").and_then(Value::as_str),
        ) {
            if !name.trim().is_empty() {
                names.insert(id.to_string(), clean_title(name));
            }
        }
    }
    names
}

/// Scans every session on disk. Blocking; run on a background thread.
pub fn scan() -> Vec<Session> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let candidates = collect_candidates(&home);
    let mut cache = load_cache();
    let cached_count = cache.len();

    let mut entries: Vec<CacheEntry> = Vec::with_capacity(candidates.len());
    let mut stale: Vec<Candidate> = Vec::new();
    for candidate in candidates {
        match cache.remove(&candidate.path) {
            Some(entry) if entry.mtime == candidate.mtime && entry.size == candidate.size => {
                entries.push(entry)
            }
            _ => stale.push(candidate),
        }
    }
    let dirty = !stale.is_empty() || entries.len() != cached_count;

    if !stale.is_empty() {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(8)
            .min(stale.len());
        let chunk_size = stale.len().div_ceil(workers);
        let parsed: Vec<CacheEntry> = std::thread::scope(|scope| {
            let handles: Vec<_> = stale
                .chunks(chunk_size)
                .map(|chunk| {
                    scope.spawn(move || {
                        chunk
                            .iter()
                            .map(|candidate| CacheEntry {
                                path: candidate.path.clone(),
                                mtime: candidate.mtime,
                                size: candidate.size,
                                session: parse_file(candidate),
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| handle.join().unwrap_or_default())
                .collect()
        });
        entries.extend(parsed);
    }

    if dirty {
        save_cache(&entries);
    }

    let codex_names = load_codex_names(&home);
    let mut sessions: Vec<Session> = entries
        .into_iter()
        .filter_map(|entry| entry.session)
        .map(|mut session| {
            if session.agent == Agent::Codex {
                if let Some(name) = codex_names.get(&session.id) {
                    session.title = name.clone();
                }
            }
            session.finish()
        })
        .filter(|session| !session.title.is_empty())
        .collect();
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
    sessions
}

fn parse_file(candidate: &Candidate) -> Option<Session> {
    let mut file = File::open(&candidate.path).ok()?;
    match candidate.agent {
        Agent::Claude => {
            let head = read_chunk(&mut file, 0, HEAD_BYTES.min(candidate.size))?;
            let tail = if candidate.size > HEAD_BYTES {
                let len = TAIL_BYTES.min(candidate.size - HEAD_BYTES);
                read_chunk(&mut file, candidate.size - len, len)?
            } else {
                Vec::new()
            };
            let id = candidate.path.file_stem()?.to_string_lossy().into_owned();
            let head_complete = candidate.size <= HEAD_BYTES;
            let parsed = parse_claude(&head, head_complete, &tail)?;
            Some(Session {
                agent: Agent::Claude,
                id,
                title: parsed.title,
                cwd: parsed
                    .cwd
                    .or_else(|| decode_claude_project_dir(&candidate.path))
                    .unwrap_or_default(),
                modified: candidate.mtime,
                path: candidate.path.clone(),
                haystack: String::new(),
            })
        }
        Agent::Codex => {
            let head = read_chunk(&mut file, 0, CODEX_HEAD_BYTES.min(candidate.size))?;
            let parsed = parse_codex(&head, candidate.size <= CODEX_HEAD_BYTES)?;
            Some(Session {
                agent: Agent::Codex,
                id: parsed.id,
                title: parsed.title,
                cwd: parsed.cwd,
                modified: candidate.mtime,
                path: candidate.path.clone(),
                haystack: String::new(),
            })
        }
    }
}

fn read_chunk(file: &mut File, offset: u64, len: u64) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = Vec::with_capacity(len as usize);
    file.take(len).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Splits a chunk into complete JSON lines, dropping a partial first or last
/// line when the chunk does not start or end at a file boundary.
fn complete_lines(chunk: &[u8], starts_at_line: bool, ends_at_line: bool) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = chunk.split(|&b| b == b'\n').collect();
    if !ends_at_line && !chunk.ends_with(b"\n") {
        lines.pop();
    }
    if !starts_at_line && !lines.is_empty() {
        lines.remove(0);
    }
    lines.retain(|line| !line.is_empty());
    lines
}

struct ClaudeParsed {
    title: String,
    cwd: Option<String>,
}

fn parse_claude(head: &[u8], head_complete: bool, tail: &[u8]) -> Option<ClaudeParsed> {
    let mut cwd = None;
    let mut first_prompt = None;
    let mut named_title = None;

    // Returns true when the whole session should be hidden.
    let mut scan_line = |line: &[u8], want_prompt: bool| -> bool {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return false;
        };
        if value
            .get("entrypoint")
            .and_then(Value::as_str)
            .is_some_and(|entry| entry.starts_with("sdk"))
        {
            // Headless SDK runs are not conversations a person resumes.
            return true;
        }
        if cwd.is_none() {
            cwd = value.get("cwd").and_then(Value::as_str).map(str::to_owned);
        }
        for key in ["customTitle", "aiTitle", "summary"] {
            if let Some(title) = value.get(key).and_then(Value::as_str) {
                if !title.trim().is_empty() {
                    // Later lines win, except a custom title is never replaced
                    // by a generated one.
                    let custom = key == "customTitle";
                    match &named_title {
                        Some((_, true)) if !custom => {}
                        _ => named_title = Some((clean_title(title), custom)),
                    }
                }
            }
        }
        if want_prompt
            && first_prompt.is_none()
            && value.get("type").and_then(Value::as_str) == Some("user")
            && value.get("isMeta").and_then(Value::as_bool) != Some(true)
            && value.get("isSidechain").and_then(Value::as_bool) != Some(true)
        {
            let content = value.get("message").and_then(|m| m.get("content"));
            first_prompt = content.and_then(prompt_text);
        }
        false
    };

    for line in complete_lines(head, true, head_complete) {
        if line.starts_with(b"{") && scan_line(line, true) {
            return None;
        }
    }
    for line in complete_lines(tail, false, true) {
        if line.starts_with(b"{") && contains(line, b"Title\"") {
            scan_line(line, false);
        }
    }

    let title = named_title.map(|(title, _)| title).or(first_prompt)?;
    Some(ClaudeParsed { title, cwd })
}

struct CodexParsed {
    id: String,
    cwd: String,
    title: String,
}

fn parse_codex(head: &[u8], head_complete: bool) -> Option<CodexParsed> {
    let lines = complete_lines(head, true, head_complete);
    let first: Value = serde_json::from_slice(lines.first()?).ok()?;
    let meta = first.get("payload").unwrap_or(&first);
    let id = meta.get("id").and_then(Value::as_str)?.to_string();
    let cwd = meta
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if meta.get("source").and_then(Value::as_str) == Some("exec") {
        return None;
    }

    let mut fallback = None;
    let mut title = None;
    for line in lines.iter().skip(1) {
        // Cheap pre-filter: most lines are tool output we never need to parse.
        let is_user_event = contains(line, b"\"user_message\"");
        let is_user_item = fallback.is_none() && contains(line, b"\"role\":\"user\"");
        if !is_user_event && !is_user_item {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let payload = value.get("payload").unwrap_or(&value);
        if payload.get("type").and_then(Value::as_str) == Some("user_message") {
            if let Some(text) = payload.get("message").and_then(Value::as_str) {
                if let Some(text) = usable_prompt(text) {
                    title = Some(text);
                    break;
                }
            }
        } else if fallback.is_none() && payload.get("role").and_then(Value::as_str) == Some("user")
        {
            fallback = payload.get("content").and_then(prompt_text);
        }
    }

    Some(CodexParsed {
        id,
        cwd,
        // Sessions with no prompt yet still get a row once the index names them.
        title: title.or(fallback).unwrap_or_default(),
    })
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Extracts the first human-written text from a message `content` value.
fn prompt_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => usable_prompt(text),
        Value::Array(blocks) => blocks.iter().find_map(|block| {
            let kind = block.get("type").and_then(Value::as_str)?;
            if kind == "text" || kind == "input_text" {
                usable_prompt(block.get("text").and_then(Value::as_str)?)
            } else {
                None
            }
        }),
        _ => None,
    }
}

/// Rejects harness-injected wrappers (`<command-name>`, AGENTS.md dumps, ...).
fn usable_prompt(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    if trimmed.is_empty()
        || trimmed.starts_with('<')
        || trimmed.starts_with("# AGENTS.md")
        || trimmed.starts_with("Caveat:")
        || trimmed.starts_with("[Request interrupted")
    {
        return None;
    }
    let title = clean_title(trimmed);
    (!title.is_empty()).then_some(title)
}

fn clean_title(text: &str) -> String {
    const MAX_CHARS: usize = 160;
    let mut out = String::new();
    let mut count = 0;
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() || c.is_control() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            count += 1;
            pending_space = false;
        }
        out.push(c);
        count += 1;
        if count >= MAX_CHARS {
            out.push('…');
            break;
        }
    }
    out
}

/// Claude encodes the working directory in the project folder name by
/// replacing `/` with `-`. Lossy, so only used when the log has no `cwd`.
fn decode_claude_project_dir(path: &Path) -> Option<String> {
    let name = path.parent()?.file_name()?.to_string_lossy().into_owned();
    Some(name.replace('-', "/"))
}

/// Formats how long ago a timestamp was, compactly.
pub fn format_age(modified: u64, now: u64) -> String {
    let secs = now.saturating_sub(modified);
    match secs {
        0..=59 => "now".to_string(),
        60..=3_599 => format!("{}m", secs / 60),
        3_600..=86_399 => format!("{}h", secs / 3_600),
        86_400..=604_799 => format!("{}d", secs / 86_400),
        604_800..=2_591_999 => format!("{}w", secs / 604_800),
        _ => format!("{}mo", secs / 2_592_000),
    }
}

/// Reads one of Claude Code's per-process records: `(pid, session id)`.
fn parse_live_record(bytes: &[u8]) -> Option<(i32, String)> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let pid = i32::try_from(value.get("pid")?.as_i64()?).ok()?;
    let id = value.get("sessionId")?.as_str()?;
    (pid > 0 && !id.is_empty()).then(|| (pid, id.to_string()))
}

/// The chat each running Claude Code process has open, by process id.
///
/// Claude Code keeps a small record per interactive process in
/// `~/.claude/sessions/<pid>.json`. That is an exact answer to "which chat
/// is this terminal running", including after `/resume` or `/clear` inside
/// the agent. Records left behind by dead processes are ignored.
pub fn live_claude_sessions() -> HashMap<i32, String> {
    let mut live = HashMap::new();
    let Some(home) = home_dir() else {
        return live;
    };
    let Ok(entries) = fs::read_dir(home.join(".claude/sessions")) else {
        return live;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Some((pid, id)) = fs::read(&path)
            .ok()
            .and_then(|bytes| parse_live_record(&bytes))
        else {
            continue;
        };
        if crate::platform::process_alive(pid) {
            live.insert(pid, id);
        }
    }
    live
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_uses_first_real_prompt_and_cwd() {
        let head = br#"{"type":"mode","mode":"normal","sessionId":"s"}
{"type":"user","isMeta":true,"message":{"role":"user","content":"<command-name>/clear</command-name>"},"cwd":"/work/app"}
{"type":"user","message":{"role":"user","content":"<local-command-stdout></local-command-stdout>"}}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Fix the\n  login bug"}]},"cwd":"/work/app"}
{"type":"user","message":{"role":"user","content":"second prompt"}}
"#;
        let parsed = parse_claude(head, true, b"").unwrap();
        assert_eq!(parsed.title, "Fix the login bug");
        assert_eq!(parsed.cwd.as_deref(), Some("/work/app"));
    }

    #[test]
    fn claude_prefers_generated_title_from_tail() {
        let head = br#"{"type":"user","message":{"role":"user","content":"hello there"},"cwd":"/w"}
"#;
        let tail = br#"partial line that is cut off"}
{"type":"ai-title","aiTitle":"Greeting the assistant","sessionId":"s"}
"#;
        let parsed = parse_claude(head, false, tail).unwrap();
        assert_eq!(parsed.title, "Greeting the assistant");
    }

    #[test]
    fn claude_custom_title_beats_generated() {
        let head = br#"{"type":"custom-title","customTitle":"My name"}
{"type":"ai-title","aiTitle":"Generated"}
"#;
        assert_eq!(parse_claude(head, true, b"").unwrap().title, "My name");
    }

    #[test]
    fn claude_skips_sdk_and_empty_sessions() {
        let sdk =
            br#"{"type":"user","entrypoint":"sdk-ts","message":{"role":"user","content":"run"}}
"#;
        assert!(parse_claude(sdk, true, b"").is_none());
        let empty = br#"{"type":"mode","mode":"normal"}
"#;
        assert!(parse_claude(empty, true, b"").is_none());
    }

    #[test]
    fn claude_ignores_truncated_last_line() {
        let head = br#"{"type":"user","message":{"role":"user","content":"real prompt"}}
{"type":"user","message":{"role":"user","content":"cut of"#;
        assert_eq!(parse_claude(head, false, b"").unwrap().title, "real prompt");
    }

    #[test]
    fn codex_reads_meta_and_user_message() {
        let head = br##"{"timestamp":"t","type":"session_meta","payload":{"id":"abc","cwd":"/work/site","source":"cli"}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions"},{"type":"input_text","text":"fallback prompt"}]}}
{"type":"event_msg","payload":{"type":"user_message","message":"Add a dark mode"}}
"##;
        let parsed = parse_codex(head, true).unwrap();
        assert_eq!(parsed.id, "abc");
        assert_eq!(parsed.cwd, "/work/site");
        assert_eq!(parsed.title, "Add a dark mode");
    }

    #[test]
    fn codex_falls_back_to_first_user_item_and_skips_exec() {
        let head = br##"{"type":"session_meta","payload":{"id":"abc","cwd":"/w"}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>x</environment_context>"},{"type":"input_text","text":"do the thing"}]}}
"##;
        assert_eq!(parse_codex(head, true).unwrap().title, "do the thing");
        let exec = br#"{"type":"session_meta","payload":{"id":"abc","cwd":"/w","source":"exec"}}
"#;
        assert!(parse_codex(exec, true).is_none());
    }

    #[test]
    fn reads_claude_process_records() {
        let record = br#"{"pid": 4242, "sessionId": "abc-123", "cwd": "/w", "status": "idle"}"#;
        assert_eq!(
            parse_live_record(record),
            Some((4242, "abc-123".to_string()))
        );
        assert_eq!(parse_live_record(br#"{"pid": 4242}"#), None);
        assert_eq!(
            parse_live_record(br#"{"pid": "x", "sessionId": "a"}"#),
            None
        );
        assert_eq!(parse_live_record(b"not json"), None);
    }

    #[test]
    fn titles_are_single_line_and_bounded() {
        assert_eq!(clean_title("  a\n\n b\tc  "), "a b c");
        let long = "x".repeat(500);
        assert_eq!(clean_title(&long).chars().count(), 161);
    }

    #[test]
    fn resume_commands_quote_ids() {
        let session = Session {
            agent: Agent::Claude,
            id: "abc-123".into(),
            title: "t".into(),
            cwd: "/w/proj/".into(),
            modified: 0,
            path: PathBuf::new(),
            haystack: String::new(),
        };
        assert_eq!(session.resume_command(), "claude --resume abc-123");
        assert_eq!(session.project_name(), "proj");
    }

    #[test]
    fn ages() {
        assert_eq!(format_age(100, 130), "now");
        assert_eq!(format_age(0, 120), "2m");
        assert_eq!(format_age(0, 7_200), "2h");
        assert_eq!(format_age(0, 172_800), "2d");
    }
}
