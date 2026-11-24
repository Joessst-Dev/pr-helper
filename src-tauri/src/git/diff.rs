//! Parses `git diff` output into an index of commentable lines.
//!
//! Both GitHub and GitLab only accept inline comments on lines that are part of a diff hunk.
//! Right side = new file (added or context lines), left side = old file (removed lines).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use crate::db::models::{DiffLineKind, FileDiffLine, Side};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Which hunk of the file the line is in, counted from 0.
    pub hunk: usize,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct FileDiff {
    pub old_path: String,
    pub new_path: String,
    /// new line number -> old line number (None for added lines).
    pub right: BTreeMap<u32, Option<u32>>,
    /// Removed old line numbers.
    pub left: BTreeSet<u32>,
    pub lines: Vec<DiffLine>,
}

impl FileDiff {
    /// The diff lines for display, at most `max`. The flag says whether lines were cut off.
    pub fn view_lines(&self, max: usize) -> (Vec<FileDiffLine>, bool) {
        let lines = self
            .lines
            .iter()
            .take(max)
            .map(|l| FileDiffLine {
                kind: l.kind.into(),
                old: l.old,
                new: l.new,
                text: l.text.clone(),
            })
            .collect();
        (lines, self.lines.len() > max)
    }
}

impl From<LineKind> for DiffLineKind {
    fn from(kind: LineKind) -> Self {
        match kind {
            LineKind::Context => Self::Context,
            LineKind::Added => Self::Added,
            LineKind::Removed => Self::Removed,
        }
    }
}

/// Where an inline comment can be attached, with both line numbers GitLab needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub old_path: String,
    pub new_path: String,
    pub side: Side,
    pub new_line: Option<u32>,
    pub old_line: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct DiffIndex {
    files: HashMap<String, FileDiff>,
}

impl DiffIndex {
    pub fn parse(diff: &str) -> Self {
        let mut files = HashMap::new();
        let mut current: Option<FileParser> = None;
        for raw in diff.lines() {
            if let Some(rest) = raw.strip_prefix("diff --git ") {
                if let Some(done) = current.replace(FileParser::new(rest)) {
                    done.finish_into(&mut files);
                }
            } else if let Some(parser) = current.as_mut() {
                parser.line(raw);
            }
        }
        if let Some(done) = current {
            done.finish_into(&mut files);
        }
        Self { files }
    }

    pub fn file(&self, path: &str) -> Option<&FileDiff> {
        self.files
            .get(path)
            .or_else(|| self.files.values().find(|f| f.old_path == path))
    }

    /// Number of changed files.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Resolve a commentable anchor. Falls back to the other side when the model picked the
    /// wrong one, e.g. a removed line reported with `side: right`.
    pub fn locate(&self, path: &str, line: u32, side: Side) -> Option<Anchor> {
        let f = self.file(path)?;
        let right = || {
            f.right.get(&line).map(|old| Anchor {
                old_path: f.old_path.clone(),
                new_path: f.new_path.clone(),
                side: Side::Right,
                new_line: Some(line),
                old_line: *old,
            })
        };
        let left = || {
            f.left.contains(&line).then(|| Anchor {
                old_path: f.old_path.clone(),
                new_path: f.new_path.clone(),
                side: Side::Left,
                new_line: None,
                old_line: Some(line),
            })
        };
        match side {
            Side::Right => right().or_else(left),
            Side::Left => left().or_else(right),
        }
    }

    /// Whether two lines of a file are in the same hunk. GitHub rejects multi-line comments
    /// whose start and end are in different hunks.
    pub fn same_hunk(&self, path: &str, start: u32, end: u32, side: Side) -> bool {
        let Some(f) = self.file(path) else {
            return false;
        };
        let hunk_of = |n: u32| {
            f.lines.iter().find_map(|l| {
                let number = match side {
                    Side::Right if l.kind != LineKind::Removed => l.new,
                    Side::Left if l.kind != LineKind::Added => l.old,
                    _ => None,
                };
                (number == Some(n)).then_some(l.hunk)
            })
        };
        matches!((hunk_of(start), hunk_of(end)), (Some(a), Some(b)) if a == b)
    }

    /// A few diff lines around the anchor, rendered as a mini unified diff for the UI.
    pub fn excerpt(&self, path: &str, line: u32, side: Side, context: usize) -> Option<String> {
        let f = self.file(path)?;
        let idx = f.lines.iter().position(|l| match side {
            Side::Right => l.new == Some(line),
            Side::Left => l.old == Some(line) && l.kind == LineKind::Removed,
        })?;
        let start = idx.saturating_sub(context);
        let end = (idx + context + 1).min(f.lines.len());
        let mut out = String::new();
        for l in &f.lines[start..end] {
            let mark = match l.kind {
                LineKind::Context => ' ',
                LineKind::Added => '+',
                LineKind::Removed => '-',
            };
            let no = l.new.or(l.old).unwrap_or(0);
            if !out.is_empty() {
                out.push('\n');
            }
            let _ = write!(out, "{mark}{no:>5} {}", l.text);
        }
        Some(out)
    }
}

/// Parser state for one file's section of a unified diff.
struct FileParser {
    file: FileDiff,
    old_no: u32,
    new_no: u32,
    in_hunk: bool,
    hunk: usize,
}

impl FileParser {
    /// Starts a file from the rest of its `diff --git` line.
    fn new(header: &str) -> Self {
        let (old_path, new_path) = split_git_header(header);
        Self {
            file: FileDiff {
                old_path,
                new_path,
                ..Default::default()
            },
            old_no: 0,
            new_no: 0,
            in_hunk: false,
            hunk: 0,
        }
    }

    fn line(&mut self, raw: &str) {
        if !self.in_hunk || raw.starts_with("@@") {
            self.header_line(raw);
        } else {
            self.hunk_line(raw);
        }
    }

    fn header_line(&mut self, raw: &str) {
        let file = &mut self.file;
        // `/dev/null` marks an added or deleted file: keep the header's path for that
        // side, since GitHub and GitLab both expect the file's real path there.
        if let Some(p) = raw.strip_prefix("--- ") {
            if let Some(p) = strip_prefix_path(p, "a/") {
                file.old_path = p;
            }
        } else if let Some(p) = raw.strip_prefix("+++ ") {
            if let Some(p) = strip_prefix_path(p, "b/") {
                file.new_path = p;
            }
        } else if let Some(p) = raw.strip_prefix("rename from ") {
            file.old_path = unquote(p);
        } else if let Some(p) = raw.strip_prefix("rename to ") {
            file.new_path = unquote(p);
        } else if raw.starts_with("@@")
            && let Some((old, new)) = parse_hunk_header(raw)
        {
            self.old_no = old;
            self.new_no = new;
            self.in_hunk = true;
            // Lines are pushed before the next header, so the first hunk is 0.
            self.hunk = file.lines.last().map_or(0, |l| l.hunk + 1);
        }
    }

    fn hunk_line(&mut self, raw: &str) {
        let (old, new) = (self.old_no, self.new_no);
        match raw.as_bytes().first() {
            Some(b' ') | None => {
                self.file.right.insert(new, Some(old));
                self.push(
                    LineKind::Context,
                    Some(old),
                    Some(new),
                    raw.get(1..).unwrap_or(""),
                );
                self.old_no += 1;
                self.new_no += 1;
            }
            Some(b'+') => {
                self.file.right.insert(new, None);
                self.push(LineKind::Added, None, Some(new), &raw[1..]);
                self.new_no += 1;
            }
            Some(b'-') => {
                self.file.left.insert(old);
                self.push(LineKind::Removed, Some(old), None, &raw[1..]);
                self.old_no += 1;
            }
            Some(b'\\') => {} // "\ No newline at end of file"
            _ => self.in_hunk = false,
        }
    }

    fn push(&mut self, kind: LineKind, old: Option<u32>, new: Option<u32>, text: &str) {
        self.file.lines.push(DiffLine {
            hunk: self.hunk,
            kind,
            old,
            new,
            text: text.to_string(),
        });
    }

    /// Files are keyed by their new path, or the old one for a deleted file.
    fn finish_into(self, files: &mut HashMap<String, FileDiff>) {
        let f = self.file;
        let key = if f.new_path.is_empty() {
            f.old_path.clone()
        } else {
            f.new_path.clone()
        };
        files.insert(key, f);
    }
}

fn strip_prefix_path(p: &str, prefix: &str) -> Option<String> {
    let p = p.trim_end_matches('\t').trim();
    if p == "/dev/null" {
        return None;
    }
    let mut p = unquote(p);
    if p.starts_with(prefix) {
        p.drain(..prefix.len());
    }
    Some(p)
}

/// Undoes git's C-style path quoting (`"caf\303\251.rs"`, `"a\tb"`), which it applies to
/// paths with control characters, quotes or backslashes (and non-ASCII unless
/// `core.quotePath=false`).
fn unquote(p: &str) -> String {
    let Some(inner) = p.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')) else {
        return p.to_string();
    };
    let bytes = inner.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let next = bytes[i + 1];
        // Git writes each byte of a non-ASCII path as three octal digits (at most \377).
        let octal = bytes
            .get(i + 1..i + 4)
            .filter(|d| d.iter().all(|b| (b'0'..=b'7').contains(b)))
            .and_then(|d| {
                let value = u32::from(d[0] - b'0') * 64
                    + u32::from(d[1] - b'0') * 8
                    + u32::from(d[2] - b'0');
                u8::try_from(value).ok()
            });
        if let Some(byte) = octal {
            out.push(byte);
            i += 4;
            continue;
        }
        out.push(match next {
            b'n' => b'\n',
            b't' => b'\t',
            b'r' => b'\r',
            b'a' => 0x07,
            b'b' => 0x08,
            b'f' => 0x0c,
            b'v' => 0x0b,
            other => other,
        });
        i += 2;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `a/foo b/foo` -> (foo, foo). Paths with spaces are ambiguous here, but the `---`/`+++`
/// lines that follow overwrite these values for every file with content changes.
fn split_git_header(rest: &str) -> (String, String) {
    let rest = rest.trim();
    if let Some(idx) = rest.find(" b/") {
        let a = rest[..idx].trim_start_matches("a/").to_string();
        let b = rest[idx + 3..].to_string();
        (a, b)
    } else {
        (rest.to_string(), rest.to_string())
    }
}

/// `@@ -12,5 +14,7 @@ fn x()` -> (12, 14)
fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let mut parts = line.split_whitespace().skip(1);
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let start = |s: &str| s.split(',').next()?.parse::<u32>().ok();
    Some((start(old)?, start(new)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 111..222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,5 @@
 fn a() {}
-fn b() {}
+fn b() -> u8 { 1 }
+fn c() {}
 fn d() {}
 fn e() {}
@@ -20,2 +21,2 @@ impl X {
 keep
-old
+new
diff --git a/old_name.rs b/new_name.rs
similarity index 90%
rename from old_name.rs
rename to new_name.rs
--- a/old_name.rs
+++ b/new_name.rs
@@ -3 +3 @@
-x
+y
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-bye
-now
";

    #[test]
    fn indexes_right_and_left_lines() {
        let idx = DiffIndex::parse(DIFF);
        let f = idx.file("src/lib.rs").unwrap();
        assert_eq!(f.right.get(&1), Some(&Some(1))); // context
        assert_eq!(f.right.get(&2), Some(&None)); // added
        assert_eq!(f.right.get(&3), Some(&None));
        assert_eq!(f.right.get(&4), Some(&Some(3)));
        assert!(f.left.contains(&2));
        assert_eq!(f.right.get(&21), Some(&Some(20)));
        assert_eq!(f.right.get(&22), Some(&None));
        assert!(f.left.contains(&21));
        assert!(!f.right.contains_key(&10));
    }

    #[test]
    fn locate_handles_sides_and_renames() {
        let idx = DiffIndex::parse(DIFF);
        let a = idx.locate("src/lib.rs", 4, Side::Right).unwrap();
        assert_eq!((a.new_line, a.old_line), (Some(4), Some(3)));
        // Removed line reported on the wrong side falls back to left.
        let a = idx.locate("src/lib.rs", 21, Side::Right).unwrap();
        assert_eq!(a.side, Side::Right); // line 21 exists on the right as context
        let a = idx.locate("src/lib.rs", 2, Side::Left).unwrap();
        assert_eq!(a.side, Side::Left);
        assert!(idx.locate("src/lib.rs", 10, Side::Right).is_none());

        let r = idx.locate("new_name.rs", 3, Side::Right).unwrap();
        assert_eq!(r.old_path, "old_name.rs");
        assert_eq!(r.new_path, "new_name.rs");
        assert!(idx.file("old_name.rs").is_some());

        let d = idx.locate("gone.txt", 1, Side::Right).unwrap();
        assert_eq!(d.side, Side::Left);
        assert_eq!(d.old_line, Some(1));
    }

    #[test]
    fn excerpt_shows_surrounding_lines() {
        let idx = DiffIndex::parse(DIFF);
        let ex = idx.excerpt("src/lib.rs", 3, Side::Right, 1).unwrap();
        assert_eq!(
            ex,
            "+    2 fn b() -> u8 { 1 }\n+    3 fn c() {}\n     4 fn d() {}"
        );
    }

    #[test]
    fn view_lines_maps_kinds_and_caps_length() {
        let idx = DiffIndex::parse(DIFF);
        let f = idx.file("src/lib.rs").unwrap();
        let (lines, cut) = f.view_lines(100);
        assert!(!cut);
        assert_eq!(lines.len(), f.lines.len());
        assert_eq!(lines[1].kind, DiffLineKind::Removed);
        assert_eq!((lines[1].old, lines[1].new), (Some(2), None));
        assert_eq!(lines[2].kind, DiffLineKind::Added);
        assert_eq!(lines[2].text, "fn b() -> u8 { 1 }");
        let (short, cut) = f.view_lines(2);
        assert!(cut);
        assert_eq!(short.len(), 2);
    }

    #[test]
    fn same_hunk_tells_hunks_apart() {
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n\
@@ -1,2 +1,2 @@\n x\n-old\n+new\n@@ -40,2 +40,2 @@\n y\n-old2\n+new2\n";
        let index = DiffIndex::parse(diff);
        assert!(index.same_hunk("a.rs", 1, 2, Side::Right));
        assert!(!index.same_hunk("a.rs", 2, 41, Side::Right));
        assert!(!index.same_hunk("a.rs", 2, 41, Side::Left));
        assert!(!index.same_hunk("a.rs", 2, 99, Side::Right));
    }

    #[test]
    fn hunk_header_parsing() {
        assert_eq!(
            parse_hunk_header("@@ -12,5 +14,7 @@ fn x()"),
            Some((12, 14))
        );
        assert_eq!(parse_hunk_header("@@ -3 +3 @@"), Some((3, 3)));
        assert_eq!(parse_hunk_header("@@ garbage"), None);
    }

    #[test]
    fn added_and_deleted_files_keep_their_path_on_both_sides() {
        let diff = "diff --git a/gone.txt b/gone.txt\ndeleted file mode 100644\n--- a/gone.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-one\n-two\ndiff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello\n";
        let idx = DiffIndex::parse(diff);
        let gone = idx.file("gone.txt").expect("deleted file indexed");
        assert_eq!(
            (gone.old_path.as_str(), gone.new_path.as_str()),
            ("gone.txt", "gone.txt")
        );
        let new = idx.file("new.txt").expect("added file indexed");
        assert_eq!(
            (new.old_path.as_str(), new.new_path.as_str()),
            ("new.txt", "new.txt")
        );
    }

    #[test]
    fn quoted_paths_are_unescaped() {
        assert_eq!(unquote("\"a/caf\\303\\251.rs\""), "a/café.rs");
        assert_eq!(unquote("\"tab\\there \\\"q\\\"\""), "tab\there \"q\"");
        assert_eq!(unquote("plain.rs"), "plain.rs");
        let diff = "diff --git \"a/caf\\303\\251.rs\" \"b/caf\\303\\251.rs\"\n--- \"a/caf\\303\\251.rs\"\n+++ \"b/caf\\303\\251.rs\"\n@@ -1 +1 @@\n-a\n+b\n";
        assert!(DiffIndex::parse(diff).file("café.rs").is_some());
    }
}
