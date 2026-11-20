//! Detects file paths and URLs in a line of terminal text.
//!
//! Agents print paths in many shapes (`src/main.rs:42`, `Read(app/page.tsx)`,
//! `"a file.txt"`, `b/src/lib.rs`), so instead of guessing with a grammar we
//! enumerate candidate substrings around the pointer and keep the longest one
//! that actually exists on disk.

use std::ops::Range;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    File {
        path: PathBuf,
        is_dir: bool,
        line: Option<u32>,
        column: Option<u32>,
    },
    Url(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// Char range of the link within the line.
    pub range: Range<usize>,
    pub target: LinkTarget,
}

/// Answers whether a path exists: `Some(true)` for directories,
/// `Some(false)` for files, `None` when missing.
pub type Probe<'a> = &'a dyn Fn(&Path) -> Option<bool>;

pub fn fs_probe(path: &Path) -> Option<bool> {
    std::fs::metadata(path).ok().map(|m| m.is_dir())
}

const MAX_CANDIDATES: usize = 96;

fn is_hard_delimiter(c: char) -> bool {
    c.is_whitespace() || matches!(c, '│' | '┃' | '║' | '|' | '<' | '>' | '⏺' | '⎿' | '•')
}

fn is_soft_delimiter(c: char) -> bool {
    matches!(
        c,
        '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`' | ',' | ';' | ':' | '=' | '*' | '#'
    )
}

/// Finds the link under `index` (a char offset into `line`).
pub fn find_link_at(line: &[char], index: usize, bases: &[PathBuf], probe: Probe) -> Option<Link> {
    if index >= line.len() || is_hard_delimiter(line[index]) {
        return None;
    }
    let mut start = index;
    while start > 0 && !is_hard_delimiter(line[start - 1]) {
        start -= 1;
    }
    let mut end = index + 1;
    while end < line.len() && !is_hard_delimiter(line[end]) {
        end += 1;
    }

    if let Some(link) = find_url(line, start, end, index) {
        return Some(link);
    }
    if let Some(link) = find_path_in_token(line, start, end, index, bases, probe) {
        return Some(link);
    }
    find_quoted_path(line, index, bases, probe)
}

fn find_url(line: &[char], start: usize, end: usize, index: usize) -> Option<Link> {
    let token: String = line[start..end].iter().collect();
    let scheme_at = ["https://", "http://", "file://"]
        .iter()
        .filter_map(|scheme| token.find(scheme))
        .min()?;
    let url_start = start + token[..scheme_at].chars().count();
    let mut url_end = end;
    while url_end > url_start {
        let c = line[url_end - 1];
        let unbalanced_paren = c == ')' && !line[url_start..url_end - 1].contains(&'(');
        if matches!(
            c,
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '`' | ']' | '}' | '*'
        ) || unbalanced_paren
        {
            url_end -= 1;
        } else {
            break;
        }
    }
    if index < url_start || index >= url_end {
        return None;
    }
    let url: String = line[url_start..url_end].iter().collect();
    let target = match url.strip_prefix("file://") {
        Some(path) => LinkTarget::File {
            path: crate::platform::local_path(&percent_decode(path), cfg!(windows)),
            is_dir: false,
            line: None,
            column: None,
        },
        None => LinkTarget::Url(url),
    };
    Some(Link {
        range: url_start..url_end,
        target,
    })
}

fn find_path_in_token(
    line: &[char],
    start: usize,
    end: usize,
    index: usize,
    bases: &[PathBuf],
    probe: Probe,
) -> Option<Link> {
    let mut starts = vec![start];
    for i in start..=index {
        if is_soft_delimiter(line[i]) && i + 1 <= index {
            starts.push(i + 1);
        }
    }
    let mut ends = vec![end];
    for j in (index + 1..end).rev() {
        if is_soft_delimiter(line[j]) {
            ends.push(j);
        }
    }

    let mut tried = 0;
    for &s in &starts {
        for &e in &ends {
            // Sentences end in dots; paths rarely do.
            let mut e = e;
            while e > index + 1 && line[e - 1] == '.' {
                e -= 1;
            }
            if e <= s {
                continue;
            }
            tried += 1;
            if tried > MAX_CANDIDATES {
                return None;
            }
            let candidate: String = line[s..e].iter().collect();
            if let Some((path, is_dir)) = resolve(&candidate, bases, probe) {
                let (line_no, column, suffix_len) = parse_position(&line[e..]);
                return Some(Link {
                    range: s..e + suffix_len,
                    target: LinkTarget::File {
                        path,
                        is_dir,
                        line: line_no,
                        column,
                    },
                });
            }
        }
    }
    None
}

/// Handles paths containing spaces when they are wrapped in quotes or backticks.
fn find_quoted_path(line: &[char], index: usize, bases: &[PathBuf], probe: Probe) -> Option<Link> {
    for quote in ['`', '"', '\''] {
        let Some(open) = line[..index].iter().rposition(|&c| c == quote) else {
            continue;
        };
        let Some(close) = line[index..].iter().position(|&c| c == quote) else {
            continue;
        };
        let close = index + close;
        if close <= open + 1 || close - open > 400 {
            continue;
        }
        let candidate: String = line[open + 1..close].iter().collect();
        if let Some((path, is_dir)) = resolve(&candidate, bases, probe) {
            return Some(Link {
                range: open + 1..close,
                target: LinkTarget::File {
                    path,
                    is_dir,
                    line: None,
                    column: None,
                },
            });
        }
    }
    None
}

fn resolve(candidate: &str, bases: &[PathBuf], probe: Probe) -> Option<(PathBuf, bool)> {
    let trimmed = candidate.trim();
    if trimmed.is_empty()
        || trimmed.len() > 1024
        || trimmed
            .chars()
            .all(|c| matches!(c, '.' | '/' | '-' | '~' | '\\'))
        || !trimmed.chars().any(|c| c.is_alphanumeric())
    {
        return None;
    }

    let mut variants: Vec<&str> = vec![trimmed];
    // Git diff headers prefix paths with `a/` and `b/`.
    if let Some(rest) = trimmed
        .strip_prefix("a/")
        .or_else(|| trimmed.strip_prefix("b/"))
    {
        variants.push(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("./") {
        variants.push(rest);
    }
    if cfg!(windows) {
        if let Some(rest) = trimmed.strip_prefix(".\\") {
            variants.push(rest);
        }
    }

    for variant in variants {
        let in_home = variant
            .strip_prefix("~/")
            .or_else(|| cfg!(windows).then(|| variant.strip_prefix("~\\")).flatten());
        if let Some(rest) = in_home {
            if let Some(home) = home_dir() {
                let path = home.join(rest);
                if let Some(is_dir) = probe(&path) {
                    return Some((normalize(&path), is_dir));
                }
            }
            continue;
        }
        let relative = Path::new(variant);
        if relative.is_absolute() {
            if let Some(is_dir) = probe(relative) {
                return Some((normalize(relative), is_dir));
            }
            continue;
        }
        for base in bases {
            let path = base.join(relative);
            if let Some(is_dir) = probe(&path) {
                return Some((normalize(&path), is_dir));
            }
        }
    }
    None
}

/// Parses `:12`, `:12:5`, `:12-20`, `#L12` and `(12, 5)` suffixes.
/// Returns `(line, column, chars consumed)`.
fn parse_position(rest: &[char]) -> (Option<u32>, Option<u32>, usize) {
    fn number(chars: &[char], at: usize) -> Option<(u32, usize)> {
        let mut end = at;
        while end < chars.len() && chars[end].is_ascii_digit() && end - at < 9 {
            end += 1;
        }
        if end == at {
            return None;
        }
        let text: String = chars[at..end].iter().collect();
        text.parse().ok().map(|n| (n, end))
    }

    match rest.first() {
        Some(':') => {
            let Some((line, mut end)) = number(rest, 1) else {
                return (None, None, 0);
            };
            let mut column = None;
            if rest.get(end) == Some(&':') {
                if let Some((col, col_end)) = number(rest, end + 1) {
                    column = Some(col);
                    end = col_end;
                }
            } else if rest.get(end) == Some(&'-') {
                if let Some((_, range_end)) = number(rest, end + 1) {
                    end = range_end;
                }
            }
            (Some(line), column, end)
        }
        Some('#') if rest.get(1) == Some(&'L') => match number(rest, 2) {
            Some((line, end)) => (Some(line), None, end),
            None => (None, None, 0),
        },
        Some('(') => {
            let Some((line, mut end)) = number(rest, 1) else {
                return (None, None, 0);
            };
            let mut column = None;
            if rest.get(end) == Some(&',') {
                let mut at = end + 1;
                while rest.get(at) == Some(&' ') {
                    at += 1;
                }
                if let Some((col, col_end)) = number(rest, at) {
                    column = Some(col);
                    end = col_end;
                }
            }
            if rest.get(end) == Some(&')') {
                (Some(line), column, end + 1)
            } else {
                (None, None, 0)
            }
        }
        _ => (None, None, 0),
    }
}

pub use crate::platform::home_dir;

/// Lexically removes `.` and `..` so the same file always maps to one tab.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn percent_decode(input: &str) -> String {
    fn hex(byte: u8) -> Option<u8> {
        (byte as char).to_digit(16).map(|d| d as u8)
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe_for(
        files: &'static [&'static str],
        dirs: &'static [&'static str],
    ) -> impl Fn(&Path) -> Option<bool> {
        move |path: &Path| {
            if dirs.iter().any(|d| Path::new(d) == path) {
                Some(true)
            } else if files.iter().any(|f| Path::new(f) == path) {
                Some(false)
            } else {
                None
            }
        }
    }

    fn link_at(text: &str, needle: &str, probe: Probe) -> Option<Link> {
        let chars: Vec<char> = text.chars().collect();
        let byte = text.find(needle).expect("needle in text");
        let index = text[..byte].chars().count() + needle.chars().count() / 2;
        find_link_at(&chars, index, &[PathBuf::from("/repo")], probe)
    }

    fn file(path: &str, line: Option<u32>, column: Option<u32>) -> LinkTarget {
        LinkTarget::File {
            path: PathBuf::from(path),
            is_dir: false,
            line,
            column,
        }
    }

    #[test]
    fn relative_path_with_line_and_column() {
        let probe = probe_for(&["/repo/src/main.rs"], &[]);
        let link = link_at("error at src/main.rs:42:7 here", "main", &probe).unwrap();
        assert_eq!(link.target, file("/repo/src/main.rs", Some(42), Some(7)));
        let text: Vec<char> = "error at src/main.rs:42:7 here".chars().collect();
        let matched: String = text[link.range].iter().collect();
        assert_eq!(matched, "src/main.rs:42:7");
    }

    #[test]
    fn tool_call_wrapping() {
        let probe = probe_for(&["/repo/src/app.rs"], &[]);
        let link = link_at("⏺ Update(src/app.rs)", "app", &probe).unwrap();
        assert_eq!(link.target, file("/repo/src/app.rs", None, None));
    }

    #[test]
    fn path_with_brackets_and_parens() {
        let probe = probe_for(&["/repo/app/(site)/[id]/page.tsx"], &[]);
        let link = link_at("Read(app/(site)/[id]/page.tsx)", "page", &probe).unwrap();
        assert_eq!(
            link.target,
            file("/repo/app/(site)/[id]/page.tsx", None, None)
        );
    }

    #[test]
    fn absolute_path_and_trailing_period() {
        let probe = probe_for(&["/etc/hosts"], &[]);
        let link = link_at("See /etc/hosts.", "hosts", &probe).unwrap();
        assert_eq!(link.target, file("/etc/hosts", None, None));
    }

    #[test]
    fn git_diff_prefix() {
        let probe = probe_for(&["/repo/src/lib.rs"], &[]);
        let link = link_at("+++ b/src/lib.rs", "lib", &probe).unwrap();
        assert_eq!(link.target, file("/repo/src/lib.rs", None, None));
    }

    #[test]
    fn quoted_path_with_spaces() {
        let probe = probe_for(&["/repo/my notes/todo list.md"], &[]);
        let link = link_at("opened `my notes/todo list.md` ok", "todo", &probe).unwrap();
        assert_eq!(link.target, file("/repo/my notes/todo list.md", None, None));
    }

    #[test]
    fn hash_line_suffix_and_paren_position() {
        let probe = probe_for(&["/repo/a.ts"], &[]);
        assert_eq!(
            link_at("a.ts#L12", "a.ts", &probe).unwrap().target,
            file("/repo/a.ts", Some(12), None)
        );
        assert_eq!(
            link_at("a.ts(3, 9): error", "a.ts", &probe).unwrap().target,
            file("/repo/a.ts", Some(3), Some(9))
        );
    }

    #[test]
    fn directories_resolve() {
        let probe = probe_for(&[], &["/repo/src"]);
        let link = link_at("ls src/", "src", &probe).unwrap();
        assert_eq!(
            link.target,
            LinkTarget::File {
                path: PathBuf::from("/repo/src"),
                is_dir: true,
                line: None,
                column: None
            }
        );
    }

    #[test]
    fn urls() {
        let probe = probe_for(&[], &[]);
        let link = link_at("docs (https://example.com/a?b=1).", "example", &probe).unwrap();
        assert_eq!(
            link.target,
            LinkTarget::Url("https://example.com/a?b=1".into())
        );
    }

    #[test]
    fn missing_files_are_not_links() {
        let probe = probe_for(&[], &[]);
        assert!(link_at("nothing src/nope.rs here", "nope", &probe).is_none());
        assert!(link_at("plain words", "words", &probe).is_none());
    }
}
