//! Finds files in a folder by name, and lines in them by what they say.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::find;
use crate::platform;

/// Folders a search never looks inside when git cannot say what to skip.
const SKIPPED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "__pycache__",
    "vendor",
    ".venv",
    "venv",
    ".next",
    ".cache",
    ".idea",
    ".gradle",
];
const MAX_FILES: usize = 200_000;
/// Bigger files are data more often than something to read.
const MAX_TEXT_BYTES: u64 = 1024 * 1024;
const MAX_LINES_PER_FILE: usize = 20;
pub const MAX_LINES: usize = 400;
const MAX_LINE_CHARS: usize = 200;
/// Shorter text matches nearly every file.
pub const MIN_TEXT_QUERY: usize = 2;

/// A file a search can find.
#[derive(Clone, Debug, PartialEq)]
pub struct Indexed {
    pub path: PathBuf,
    /// The path within the folder, with `/` between its parts.
    pub relative: String,
    lower: String,
    /// Where the file's own name starts in `relative`.
    name_start: usize,
}

impl Indexed {
    pub fn new(root: &Path, relative: &str) -> Self {
        let relative = relative.replace('\\', "/");
        Indexed {
            path: root.join(&relative),
            lower: relative.to_lowercase(),
            name_start: relative.rfind('/').map_or(0, |slash| slash + 1),
            relative,
        }
    }

    pub fn name(&self) -> &str {
        &self.relative[self.name_start..]
    }

    /// The folder the file is in, relative to the root; empty at the top.
    pub fn folder(&self) -> &str {
        self.relative[..self.name_start].trim_end_matches('/')
    }
}

/// Every file under `root` worth searching, in path order. Blocking.
pub fn list_files(root: &Path) -> Vec<Indexed> {
    let mut relative = git_files(root).unwrap_or_else(|| walked_files(root));
    relative.truncate(MAX_FILES);
    relative.sort_unstable();
    relative
        .iter()
        .map(|relative| Indexed::new(root, relative))
        .collect()
}

/// What git tracks or would track: everything it has not been told to ignore.
fn git_files(root: &Path) -> Option<Vec<String>> {
    let output = platform::command("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut files: Vec<String> = output
        .stdout
        .split(|&byte| byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect();
    // A file can be listed twice while a merge is under way.
    files.sort_unstable();
    files.dedup();
    Some(files)
}

fn walked_files(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if out.len() >= MAX_FILES {
                return;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if kind.is_dir() {
                if !SKIPPED_DIRS.contains(&name.as_str()) {
                    walk(&entry.path(), &relative, out);
                }
            } else if kind.is_file() && name != ".DS_Store" {
                out.push(relative);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", &mut out);
    out
}

/// The files whose paths hold every word of `query`, best first: a match in
/// the file's own name beats one in its folders, and shorter paths lead.
pub fn name_matches(files: &[Indexed], query: &str, limit: usize) -> Vec<usize> {
    let query = query.to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<(u8, usize, usize)> = files
        .iter()
        .enumerate()
        .filter(|(_, file)| words.iter().all(|word| file.lower.contains(word)))
        .map(|(index, file)| {
            let name = &file.lower[file.name_start..];
            let rank = if name.starts_with(words[0]) {
                0
            } else if words.iter().all(|word| name.contains(word)) {
                1
            } else {
                2
            };
            (rank, file.relative.len(), index)
        })
        .collect();
    found.sort_unstable();
    found.truncate(limit);
    found.into_iter().map(|(_, _, index)| index).collect()
}

/// A line that holds the text searched for.
#[derive(Clone, Debug, PartialEq)]
pub struct LineHit {
    /// Index of the file among those searched.
    pub file: usize,
    /// Counted from 1.
    pub line: u32,
    /// The line without its indentation, cut if it is long.
    pub text: String,
    /// Where the match is in `text`, when the cut left it in.
    pub found: Option<Range<usize>>,
}

/// Whether `needle` occurs in `hay`; `needle` is ASCII lowercase when the
/// search ignores case.
fn contains_bytes(hay: &[u8], needle: &[u8], case_sensitive: bool) -> bool {
    let Some((&first, rest)) = needle.split_first() else {
        return true;
    };
    if hay.len() < needle.len() {
        return false;
    }
    let same = |a: u8, b: u8| a == b || (!case_sensitive && a.to_ascii_lowercase() == b);
    (0..=hay.len() - needle.len()).any(|start| {
        same(hay[start], first)
            && hay[start + 1..start + needle.len()]
                .iter()
                .zip(rest)
                .all(|(&a, &b)| same(a, b))
    })
}

/// The lines of `text` that hold `query`, for the file at `file`.
fn lines_in(text: &str, query: &str, case_sensitive: bool, file: usize) -> Vec<LineHit> {
    let mut hits = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let Some(found) = find::find_in_line(line, query, case_sensitive)
            .into_iter()
            .next()
        else {
            continue;
        };
        let indent = line.len() - line.trim_start().len();
        let shown = line.trim();
        let cut = shown
            .char_indices()
            .nth(MAX_LINE_CHARS)
            .map_or(shown.len(), |(end, _)| end);
        let found = (found.start >= indent && found.end - indent <= cut)
            .then(|| found.start - indent..found.end - indent);
        hits.push(LineHit {
            file,
            line: number as u32 + 1,
            text: shown[..cut].to_string(),
            found,
        });
        if hits.len() == MAX_LINES_PER_FILE {
            break;
        }
    }
    hits
}

/// The lines in `files` that hold `query`, in file order. Lowercase text
/// ignores case. Blocking; `cancel` stops it early.
pub fn text_matches(files: &[Indexed], query: &str, cancel: &AtomicBool) -> Vec<LineHit> {
    if query.trim().chars().count() < MIN_TEXT_QUERY || files.is_empty() {
        return Vec::new();
    }
    let case_sensitive = find::is_case_sensitive(query);
    // A quick look through the raw bytes rules most files out.
    let needle = query.is_ascii().then(|| {
        if case_sensitive {
            query.as_bytes().to_vec()
        } else {
            query.to_ascii_lowercase().into_bytes()
        }
    });
    let found = AtomicUsize::new(0);

    let search_file = |index: usize| -> Vec<LineHit> {
        let file = &files[index];
        let small = fs::metadata(&file.path).is_ok_and(|meta| meta.len() <= MAX_TEXT_BYTES);
        let Some(bytes) = small.then(|| fs::read(&file.path).ok()).flatten() else {
            return Vec::new();
        };
        let binary = bytes[..bytes.len().min(8192)].contains(&0);
        let ruled_out = needle
            .as_deref()
            .is_some_and(|needle| !contains_bytes(&bytes, needle, case_sensitive));
        if binary || ruled_out {
            return Vec::new();
        }
        lines_in(&String::from_utf8_lossy(&bytes), query, case_sensitive, index)
    };

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8);
    let chunk = files.len().div_ceil(workers).max(1);
    let indices: Vec<usize> = (0..files.len()).collect();
    let mut hits: Vec<LineHit> = std::thread::scope(|scope| {
        let (found, search_file) = (&found, &search_file);
        let handles: Vec<_> = indices
            .chunks(chunk)
            .map(|chunk| {
                scope.spawn(move || {
                    let mut hits = Vec::new();
                    for &index in chunk {
                        // Enough to fill the list: no need to read the rest.
                        if cancel.load(Ordering::Relaxed)
                            || found.load(Ordering::Relaxed) >= MAX_LINES
                        {
                            break;
                        }
                        let lines = search_file(index);
                        found.fetch_add(lines.len(), Ordering::Relaxed);
                        hits.extend(lines);
                    }
                    hits
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect()
    });
    hits.truncate(MAX_LINES);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<Indexed> {
        paths
            .iter()
            .map(|path| Indexed::new(Path::new("/work"), path))
            .collect()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nexus-file-search-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn paths_split_into_folder_and_name() {
        let file = Indexed::new(Path::new("/work"), r"src\terminal\mod.rs");
        assert_eq!(file.relative, "src/terminal/mod.rs");
        assert_eq!(file.name(), "mod.rs");
        assert_eq!(file.folder(), "src/terminal");
        assert_eq!(file.path, PathBuf::from("/work/src/terminal/mod.rs"));
        let top = Indexed::new(Path::new("/work"), "README.md");
        assert_eq!((top.name(), top.folder()), ("README.md", ""));
    }

    #[test]
    fn names_rank_above_folders_and_short_paths_lead() {
        let files = files(&[
            "docs/main/notes.txt",
            "src/domain.rs",
            "src/main.rs",
            "src/bin/main_helper.rs",
            "README.md",
        ]);
        let found = |query: &str| -> Vec<&str> {
            name_matches(&files, query, 10)
                .into_iter()
                .map(|index| files[index].relative.as_str())
                .collect()
        };
        assert_eq!(
            found("main"),
            vec![
                "src/main.rs",
                "src/bin/main_helper.rs",
                "src/domain.rs",
                "docs/main/notes.txt"
            ]
        );
        // Every word must be somewhere in the path, in any order and case.
        assert_eq!(
            found("RS src main"),
            vec!["src/main.rs", "src/domain.rs", "src/bin/main_helper.rs"]
        );
        assert_eq!(found("docs/main"), vec!["docs/main/notes.txt"]);
        assert!(found("missing").is_empty());
        assert!(found("   ").is_empty());
        assert_eq!(name_matches(&files, "main", 2).len(), 2);
    }

    #[test]
    fn lines_keep_their_number_and_where_the_match_is() {
        let text = "fn main() {\n    let Cursor = cursor();\n}\n";
        assert_eq!(
            lines_in(text, "cursor", false, 7),
            vec![LineHit {
                file: 7,
                line: 2,
                text: "let Cursor = cursor();".into(),
                found: Some(4..10),
            }]
        );
        // A capital letter makes the search exact.
        assert!(lines_in(text, "CURSOR", true, 0).is_empty());
        assert_eq!(lines_in(text, "Cursor", true, 0)[0].found, Some(4..10));

        // A match past the cut is still a hit, just not marked.
        let long = format!("{}needle", "x".repeat(MAX_LINE_CHARS + 20));
        let hit = &lines_in(&long, "needle", false, 0)[0];
        assert_eq!((hit.text.len(), &hit.found), (MAX_LINE_CHARS, &None));

        let many = "hit\n".repeat(MAX_LINES_PER_FILE + 5);
        assert_eq!(lines_in(&many, "hit", false, 0).len(), MAX_LINES_PER_FILE);
    }

    #[test]
    fn bytes_are_matched_with_or_without_case() {
        assert!(contains_bytes(b"The Cursor moved", b"cursor", false));
        assert!(!contains_bytes(b"The Cursor moved", b"cursor", true));
        assert!(contains_bytes(b"The Cursor moved", b"Cursor", true));
        assert!(!contains_bytes(b"cur", b"cursor", false));
        assert!(contains_bytes(b"anything", b"", true));
    }

    #[test]
    fn text_is_found_across_files_and_binaries_are_skipped() {
        let dir = scratch("text");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.rs"), "let pointer = 1;\n// Pointer overlay\n").unwrap();
        fs::write(dir.join("notes.txt"), "nothing here\n").unwrap();
        fs::write(dir.join("blob.bin"), b"pointer\0\x01\x02").unwrap();
        let files: Vec<Indexed> = ["blob.bin", "notes.txt", "src/a.rs", "gone.txt"]
            .iter()
            .map(|path| Indexed::new(&dir, path))
            .collect();
        let cancel = AtomicBool::new(false);

        let hits = text_matches(&files, "pointer", &cancel);
        assert_eq!(
            hits.iter().map(|hit| (hit.file, hit.line)).collect::<Vec<_>>(),
            vec![(2, 1), (2, 2)]
        );
        assert_eq!(text_matches(&files, "Pointer", &cancel).len(), 1);
        // One letter is not worth a search.
        assert!(text_matches(&files, "p", &cancel).is_empty());
        cancel.store(true, Ordering::Relaxed);
        assert!(text_matches(&files, "pointer", &cancel).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn folders_are_walked_past_what_is_generated() {
        let dir = scratch("walk");
        for path in ["src/main.rs", "node_modules/pkg/index.js", ".git/config", "docs/a.md", ".env.example"] {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "x").unwrap();
        }
        let mut found = walked_files(&dir);
        found.sort();
        assert_eq!(found, vec![".env.example", "docs/a.md", "src/main.rs"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
