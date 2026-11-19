//! Line diffs between a file's baseline and its current text, and where that
//! baseline comes from.
//!
//! The baseline is the version in git `HEAD` when the file is tracked, so the
//! file view shows what an agent has changed since the last commit.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Give up on an optimal diff past this many edits and show the changed
/// region as one replaced block instead; keeps huge rewrites cheap.
const MAX_EDIT_DISTANCE: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Same,
    Added,
    Removed,
}

/// One displayed line of a unified diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub kind: RowKind,
    /// Index into the new lines for `Same`/`Added`, into the old lines for
    /// `Removed`.
    pub index: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diff {
    pub rows: Vec<Row>,
    pub added: usize,
    pub removed: usize,
    /// The row showing each new line.
    pub row_of_line: Vec<u32>,
    /// Whether each new line was added.
    pub line_added: Vec<bool>,
    /// The first row of each run of changes.
    pub hunks: Vec<u32>,
}

impl Diff {
    /// The new line shown at, or first after, `row`.
    pub fn line_of_row(&self, row: usize) -> usize {
        self.rows
            .iter()
            .skip(row)
            .find(|row| row.kind != RowKind::Removed)
            .map(|row| row.index as usize)
            .unwrap_or_else(|| self.row_of_line.len().saturating_sub(1))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edit {
    Keep,
    Delete,
    Insert,
}

/// Diffs two texts line by line. Returns `None` when they are identical.
pub fn diff_lines<A: AsRef<str>, B: AsRef<str>>(old: &[A], new: &[B]) -> Option<Diff> {
    // Compare interned ids rather than strings in the inner loop.
    fn intern<'a>(ids: &mut HashMap<&'a str, u32>, text: &'a str) -> u32 {
        let next = ids.len() as u32;
        *ids.entry(text).or_insert(next)
    }
    let mut ids = HashMap::new();
    let a: Vec<u32> = old
        .iter()
        .map(|line| intern(&mut ids, line.as_ref()))
        .collect();
    let b: Vec<u32> = new
        .iter()
        .map(|line| intern(&mut ids, line.as_ref()))
        .collect();
    if a == b {
        return None;
    }

    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let a_mid = &a[prefix..a.len() - suffix];
    let b_mid = &b[prefix..b.len() - suffix];

    let mut edits = vec![Edit::Keep; prefix];
    match myers(a_mid, b_mid, MAX_EDIT_DISTANCE) {
        Some(middle) => edits.extend(middle),
        None => {
            edits.extend(std::iter::repeat_n(Edit::Delete, a_mid.len()));
            edits.extend(std::iter::repeat_n(Edit::Insert, b_mid.len()));
        }
    }
    edits.extend(std::iter::repeat_n(Edit::Keep, suffix));
    Some(build(&edits, new.len()))
}

/// Myers' O(ND) shortest edit script. `None` if it needs more than `max_d` edits.
fn myers(a: &[u32], b: &[u32], max_d: usize) -> Option<Vec<Edit>> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    if n == 0 {
        return Some(vec![Edit::Insert; b.len()]);
    }
    if m == 0 {
        return Some(vec![Edit::Delete; a.len()]);
    }
    let limit = ((n + m) as usize).min(max_d) as isize;
    let offset = limit + 1;
    // v[offset + k] is the furthest x reached on diagonal k.
    let mut v = vec![0isize; (2 * limit + 3) as usize];
    // trace[d] holds diagonals -(d+1)..=(d+1) as they were before step d.
    let mut trace: Vec<Vec<i32>> = Vec::new();

    let mut found = None;
    'search: for d in 0..=limit {
        let from = (offset - d - 1) as usize;
        let to = (offset + d + 1) as usize;
        trace.push(v[from..=to].iter().map(|&x| x as i32).collect());
        let mut k = -d;
        while k <= d {
            let index = (offset + k) as usize;
            let mut x = if k == -d || (k != d && v[index - 1] < v[index + 1]) {
                v[index + 1]
            } else {
                v[index - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[index] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'search;
            }
            k += 2;
        }
    }
    let distance = found?;

    let mut edits = Vec::with_capacity((n + m) as usize);
    let (mut x, mut y) = (n, m);
    for d in (0..=distance).rev() {
        let snapshot = &trace[d as usize];
        // `snapshot[0]` is diagonal -(d+1).
        let at = |k: isize| snapshot[(k + d + 1) as usize] as isize;
        let k = x - y;
        let previous_k = if k == -d || (k != d && at(k - 1) < at(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let previous_x = at(previous_k);
        let previous_y = previous_x - previous_k;
        while x > previous_x && y > previous_y {
            edits.push(Edit::Keep);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            edits.push(if x == previous_x {
                Edit::Insert
            } else {
                Edit::Delete
            });
        }
        x = previous_x;
        y = previous_y;
    }
    edits.reverse();
    Some(edits)
}

/// Lays an edit script out as rows, showing each run of changes as its
/// removed lines followed by its added lines.
fn build(edits: &[Edit], new_len: usize) -> Diff {
    let mut diff = Diff {
        rows: Vec::with_capacity(edits.len()),
        added: 0,
        removed: 0,
        row_of_line: Vec::with_capacity(new_len),
        line_added: Vec::with_capacity(new_len),
        hunks: Vec::new(),
    };
    let (mut old_index, mut new_index) = (0u32, 0u32);
    let mut i = 0;
    while i < edits.len() {
        if edits[i] == Edit::Keep {
            diff.row_of_line.push(diff.rows.len() as u32);
            diff.line_added.push(false);
            diff.rows.push(Row {
                kind: RowKind::Same,
                index: new_index,
            });
            old_index += 1;
            new_index += 1;
            i += 1;
            continue;
        }
        let run_end = edits[i..]
            .iter()
            .position(|edit| *edit == Edit::Keep)
            .map_or(edits.len(), |offset| i + offset);
        let deletes = edits[i..run_end]
            .iter()
            .filter(|edit| **edit == Edit::Delete)
            .count() as u32;
        let inserts = (run_end - i) as u32 - deletes;
        diff.hunks.push(diff.rows.len() as u32);
        for _ in 0..deletes {
            diff.rows.push(Row {
                kind: RowKind::Removed,
                index: old_index,
            });
            old_index += 1;
        }
        for _ in 0..inserts {
            diff.row_of_line.push(diff.rows.len() as u32);
            diff.line_added.push(true);
            diff.rows.push(Row {
                kind: RowKind::Added,
                index: new_index,
            });
            new_index += 1;
        }
        diff.removed += deletes as usize;
        diff.added += inserts as usize;
        i = run_end;
    }
    diff
}

/// What a file's current contents should be compared against.
#[derive(Debug, PartialEq, Eq)]
pub enum Baseline {
    /// The file's contents in git `HEAD`.
    Head(Vec<u8>),
    /// In a git repository but not in `HEAD`: untracked or newly added.
    Untracked,
    /// Not inside a git repository.
    NoRepository,
}

pub struct GitState {
    pub baseline: Baseline,
    /// A file whose modification time changes whenever `HEAD` moves.
    pub watch: Option<PathBuf>,
}

fn git(directory: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        // Never contend with an agent's own git commands for the index lock.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

/// Looks up the committed version of `path`. Blocking; run off the UI thread.
pub fn git_state(path: &Path) -> GitState {
    let none = GitState {
        baseline: Baseline::NoRepository,
        watch: None,
    };
    let (Some(directory), Some(name)) = (path.parent(), path.file_name()) else {
        return none;
    };
    let Some(git_dir) = git(directory, &["rev-parse", "--absolute-git-dir"]) else {
        return none;
    };
    let git_dir = PathBuf::from(String::from_utf8_lossy(&git_dir).trim_end_matches('\n'));
    // The reflog is appended to on every commit, checkout and reset.
    let watch = [git_dir.join("logs/HEAD"), git_dir.join("HEAD")]
        .into_iter()
        .find(|candidate| candidate.exists());

    // `./` makes the path relative to `directory` rather than the repo root.
    let spec = format!("HEAD:./{}", name.to_string_lossy());
    let baseline = match git(directory, &["show", &spec]) {
        Some(bytes) => Baseline::Head(bytes),
        None => Baseline::Untracked,
    };
    GitState { baseline, watch }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(diff: &Diff) -> String {
        diff.rows
            .iter()
            .map(|row| match row.kind {
                RowKind::Same => ' ',
                RowKind::Added => '+',
                RowKind::Removed => '-',
            })
            .collect()
    }

    /// Rebuilds both texts from the rows to prove the diff is faithful.
    fn check(old: &[&str], new: &[&str]) -> Diff {
        let diff = diff_lines(old, new).expect("texts differ");
        let rebuilt_new: Vec<&str> = diff
            .rows
            .iter()
            .filter(|row| row.kind != RowKind::Removed)
            .map(|row| new[row.index as usize])
            .collect();
        let rebuilt_old: Vec<&str> = diff
            .rows
            .iter()
            .filter(|row| row.kind != RowKind::Added)
            .map(|row| match row.kind {
                RowKind::Removed => old[row.index as usize],
                _ => new[row.index as usize],
            })
            .collect();
        assert_eq!(rebuilt_new, new);
        assert_eq!(rebuilt_old, old);
        assert_eq!(diff.row_of_line.len(), new.len());
        for (line, &row) in diff.row_of_line.iter().enumerate() {
            assert_eq!(diff.rows[row as usize].index as usize, line);
            assert_eq!(
                diff.rows[row as usize].kind == RowKind::Added,
                diff.line_added[line]
            );
        }
        diff
    }

    #[test]
    fn identical_texts_have_no_diff() {
        assert_eq!(diff_lines(&["a", "b"], &["a", "b"]), None);
        assert_eq!(diff_lines::<&str, &str>(&[], &[]), None);
    }

    #[test]
    fn change_in_the_middle() {
        let diff = check(&["a", "b", "c", "d"], &["a", "x", "y", "d"]);
        assert_eq!(kinds(&diff), " --++ ");
        assert_eq!((diff.added, diff.removed), (2, 2));
        assert_eq!(diff.hunks, vec![1]);
        assert_eq!(diff.row_of_line, vec![0, 3, 4, 5]);
    }

    #[test]
    fn pure_insertions_and_deletions() {
        let diff = check(&["a", "c"], &["a", "b", "c"]);
        assert_eq!(kinds(&diff), " + ");
        let diff = check(&["a", "b", "c"], &["a", "c"]);
        assert_eq!(kinds(&diff), " - ");
        assert_eq!(diff.line_of_row(1), 1);
        let diff = check(&[], &["a", "b"]);
        assert_eq!(kinds(&diff), "++");
        let diff = check(&["a", "b"], &[]);
        assert_eq!(kinds(&diff), "--");
        assert_eq!(diff.line_of_row(0), 0);
    }

    #[test]
    fn separate_hunks_keep_matching_lines_between_them() {
        let old = ["fn a() {", "    1", "}", "", "fn b() {", "    2", "}"];
        let new = [
            "fn a() {", "    10", "}", "", "fn b() {", "    20", "}", "// end",
        ];
        let diff = check(&old, &new);
        assert_eq!(kinds(&diff), " -+   -+ +");
        assert_eq!(diff.hunks, vec![1, 6, 9]);
        assert_eq!((diff.added, diff.removed), (3, 2));
    }

    #[test]
    fn repeated_lines_align_sensibly() {
        let diff = check(&["x", "x", "x"], &["x", "x", "y", "x"]);
        assert_eq!((diff.added, diff.removed), (1, 0));
    }

    #[test]
    fn rewrites_past_the_edit_limit_fall_back_to_one_block() {
        let old: Vec<String> = (0..MAX_EDIT_DISTANCE + 50)
            .map(|i| format!("old {i}"))
            .collect();
        let mut new: Vec<String> = (0..MAX_EDIT_DISTANCE + 50)
            .map(|i| format!("new {i}"))
            .collect();
        new.insert(0, "same first".into());
        let mut old_with_prefix = vec!["same first".to_string()];
        old_with_prefix.extend(old);
        let diff = diff_lines(&old_with_prefix, &new).unwrap();
        assert_eq!(diff.hunks, vec![1]);
        assert_eq!(diff.added, MAX_EDIT_DISTANCE + 50);
        assert_eq!(diff.removed, MAX_EDIT_DISTANCE + 50);
    }

    #[test]
    fn random_edits_round_trip() {
        // A small deterministic generator: enough variety to shake out
        // off-by-one errors in the backtracking.
        let mut state = 0x2545_f491u32;
        let mut next = move |bound: u32| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state % bound
        };
        for _ in 0..300 {
            let old: Vec<String> = (0..next(12)).map(|_| next(4).to_string()).collect();
            let new: Vec<String> = (0..next(12)).map(|_| next(4).to_string()).collect();
            let old_refs: Vec<&str> = old.iter().map(String::as_str).collect();
            let new_refs: Vec<&str> = new.iter().map(String::as_str).collect();
            if old_refs != new_refs {
                check(&old_refs, &new_refs);
            }
        }
    }

    #[test]
    fn reads_the_committed_version_from_git() {
        let dir = std::env::temp_dir().join(format!("nexus-git-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        assert_eq!(
            git_state(&dir.join("sub/a.txt")).baseline,
            Baseline::NoRepository
        );

        let run = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        run(&["init", "-q"]);
        std::fs::write(dir.join("sub/a.txt"), "committed\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("sub/a.txt"), "edited\n").unwrap();
        std::fs::write(dir.join("sub/new.txt"), "new\n").unwrap();

        let state = git_state(&dir.join("sub/a.txt"));
        assert_eq!(state.baseline, Baseline::Head(b"committed\n".to_vec()));
        assert!(state.watch.is_some_and(|watch| watch.exists()));
        assert_eq!(
            git_state(&dir.join("sub/new.txt")).baseline,
            Baseline::Untracked
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
