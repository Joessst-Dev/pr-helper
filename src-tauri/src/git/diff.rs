//! Parses `git diff` output into an index of commentable lines.
//!
//! Both GitHub and GitLab only accept inline comments on lines that are part of a diff hunk.
//! Right side = new file (added or context lines), left side = old file (removed lines).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::db::models::Side;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
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
        let mut current: Option<FileDiff> = None;
        let (mut old_no, mut new_no) = (0u32, 0u32);
        let mut in_hunk = false;

        let flush = |f: Option<FileDiff>, files: &mut HashMap<String, FileDiff>| {
            if let Some(f) = f {
                let key = if f.new_path.is_empty() {
                    f.old_path.clone()
                } else {
                    f.new_path.clone()
                };
                files.insert(key, f);
            }
        };

        for raw in diff.lines() {
            if let Some(rest) = raw.strip_prefix("diff --git ") {
                flush(current.take(), &mut files);
                let (a, b) = split_git_header(rest);
                current = Some(FileDiff {
                    old_path: a,
                    new_path: b,
                    ..Default::default()
                });
                in_hunk = false;
                continue;
            }
            let Some(file) = current.as_mut() else {
                continue;
            };

            if !in_hunk || raw.starts_with("@@") {
                if let Some(p) = raw.strip_prefix("--- ") {
                    file.old_path = strip_prefix_path(p, "a/").unwrap_or_default();
                } else if let Some(p) = raw.strip_prefix("+++ ") {
                    file.new_path = strip_prefix_path(p, "b/").unwrap_or_default();
                } else if let Some(p) = raw.strip_prefix("rename from ") {
                    file.old_path = p.to_string();
                } else if let Some(p) = raw.strip_prefix("rename to ") {
                    file.new_path = p.to_string();
                } else if raw.starts_with("@@")
                    && let Some((o, n)) = parse_hunk_header(raw)
                {
                    old_no = o;
                    new_no = n;
                    in_hunk = true;
                }
                continue;
            }

            match raw.as_bytes().first() {
                Some(b' ') | None => {
                    file.right.insert(new_no, Some(old_no));
                    file.lines.push(DiffLine {
                        kind: LineKind::Context,
                        old: Some(old_no),
                        new: Some(new_no),
                        text: raw.get(1..).unwrap_or("").to_string(),
                    });
                    old_no += 1;
                    new_no += 1;
                }
                Some(b'+') => {
                    file.right.insert(new_no, None);
                    file.lines.push(DiffLine {
                        kind: LineKind::Added,
                        old: None,
                        new: Some(new_no),
                        text: raw[1..].to_string(),
                    });
                    new_no += 1;
                }
                Some(b'-') => {
                    file.left.insert(old_no);
                    file.lines.push(DiffLine {
                        kind: LineKind::Removed,
                        old: Some(old_no),
                        new: None,
                        text: raw[1..].to_string(),
                    });
                    old_no += 1;
                }
                Some(b'\\') => {} // "\ No newline at end of file"
                _ => in_hunk = false,
            }
        }
        flush(current.take(), &mut files);
        Self { files }
    }

    pub fn file(&self, path: &str) -> Option<&FileDiff> {
        self.files
            .get(path)
            .or_else(|| self.files.values().find(|f| f.old_path == path))
    }

    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.files.keys()
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

    /// A few diff lines around the anchor, rendered as a mini unified diff for the UI.
    pub fn excerpt(&self, path: &str, line: u32, side: Side, context: usize) -> Option<String> {
        let f = self.file(path)?;
        let idx = f.lines.iter().position(|l| match side {
            Side::Right => l.new == Some(line),
            Side::Left => l.old == Some(line) && l.kind == LineKind::Removed,
        })?;
        let start = idx.saturating_sub(context);
        let end = (idx + context + 1).min(f.lines.len());
        Some(
            f.lines[start..end]
                .iter()
                .map(|l| {
                    let mark = match l.kind {
                        LineKind::Context => ' ',
                        LineKind::Added => '+',
                        LineKind::Removed => '-',
                    };
                    let no = l.new.or(l.old).unwrap_or(0);
                    format!("{mark}{no:>5} {}", l.text)
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}

fn strip_prefix_path(p: &str, prefix: &str) -> Option<String> {
    let p = p.trim_end_matches('\t').trim();
    if p == "/dev/null" {
        return None;
    }
    let p = p.trim_matches('"');
    Some(p.strip_prefix(prefix).unwrap_or(p).to_string())
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
    fn hunk_header_parsing() {
        assert_eq!(
            parse_hunk_header("@@ -12,5 +14,7 @@ fn x()"),
            Some((12, 14))
        );
        assert_eq!(parse_hunk_header("@@ -3 +3 @@"), Some((3, 3)));
        assert_eq!(parse_hunk_header("@@ garbage"), None);
    }
}
