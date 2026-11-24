//! Deterministic enforcement of the comment rules. The model is told about every rule, but
//! nothing it returns is trusted: this runs on model output and again on user-edited text
//! right before anything is posted.
//!
//! Rules:
//! - No `--`, em dash or en dash in prose. Inline code, fenced code and URLs are left untouched.
//! - Optional: no AI attribution of any kind.
//! - Optional: drop nits.
//! - `scope` findings (unmet ticket requirements) only exist when a ticket was loaded and the
//!   scope check is on, and are never above medium.
//! - Findings sorted by severity, then file and line.

use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::db::models::{CriticalFile, Finding, FindingSource, LineRange, Severity, Side, Verdict};
use crate::db::new_id;
use crate::error::{AppError, AppResult};
use crate::git::diff::DiffIndex;
use crate::review::schema::{RawCriticalFile, RawFinding, RawReview};
use crate::settings::EffectiveSettings;

// ---- markdown segmentation --------------------------------------------------

const PH_OPEN: char = '\u{E000}';
const PH_CLOSE: char = '\u{E001}';

static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://[^\s<>()\[\]]+").expect("hard-coded regex is valid"));

/// Which lines count as code (left alone) rather than prose.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CodeBlocks {
    /// Only fenced blocks are code.
    Fenced,
    /// Fenced blocks plus markdown indented code blocks (4 spaces or a tab, after a blank line).
    /// Used for the dash rule, where rewriting code would change commands; not for attribution,
    /// which must not get an indentation loophole.
    FencedAndIndented,
}

/// Apply `f` to every prose line. Code blocks are skipped entirely; inline code spans and
/// URLs inside prose lines are swapped for placeholders while `f` runs. `f` may drop a line.
fn map_prose_lines(
    text: &str,
    blocks: CodeBlocks,
    mut f: impl FnMut(&str, &[String]) -> Option<String>,
) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    // Markdown indented code starts after a blank line and runs until a non-indented line.
    let mut prev_blank = true;
    let mut in_indented = false;
    for line in text.split('\n') {
        if fence.is_none() && blocks == CodeBlocks::FencedAndIndented {
            let indented = line.starts_with("    ") || line.starts_with('\t');
            let blank = line.trim().is_empty();
            in_indented = if blank {
                in_indented
            } else {
                indented && (prev_blank || in_indented)
            };
            prev_blank = blank;
            if in_indented && !blank {
                out.push(line.to_string());
                continue;
            }
        }
        let trimmed = line.trim_start();
        let fence_marker = ['`', '~'].into_iter().find_map(|ch| {
            let n = trimmed.chars().take_while(|c| *c == ch).count();
            (n >= 3).then_some((ch, n))
        });
        match (fence, fence_marker) {
            (None, Some(marker)) => {
                fence = Some(marker);
                out.push(line.to_string());
                continue;
            }
            (Some((ch, n)), Some((mch, mn)))
                if ch == mch && mn >= n && trimmed.trim_end().chars().all(|c| c == ch) =>
            {
                fence = None;
                out.push(line.to_string());
                continue;
            }
            (Some(_), _) => {
                out.push(line.to_string());
                continue;
            }
            (None, None) => {}
        }

        let (masked, stash) = mask_code(line);
        if let Some(new_line) = f(&masked, &stash) {
            out.push(unmask(&new_line, &stash));
        }
    }
    out.join("\n")
}

/// Visit prose fragments (code and URLs removed) without changing anything.
fn for_each_prose_line(text: &str, blocks: CodeBlocks, mut f: impl FnMut(&str, &[String])) {
    map_prose_lines(text, blocks, |l, stash| {
        f(l, stash);
        Some(l.to_string())
    });
}

fn mask_code(line: &str) -> (String, Vec<String>) {
    let mut stash = Vec::new();
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '`' {
            let run = chars[i..].iter().take_while(|c| **c == '`').count();
            // Find a closing run of exactly the same length.
            let mut j = i + run;
            let mut close = None;
            while j < chars.len() {
                if chars[j] == '`' {
                    let r = chars[j..].iter().take_while(|c| **c == '`').count();
                    if r == run {
                        close = Some(j);
                        break;
                    }
                    j += r;
                } else {
                    j += 1;
                }
            }
            if let Some(end) = close {
                let span: String = chars[i..end + run].iter().collect();
                out.push(PH_OPEN);
                out.push_str(&stash.len().to_string());
                out.push(PH_CLOSE);
                stash.push(span);
                i = end + run;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    let out = URL_RE
        .replace_all(&out, |c: &Captures| {
            let idx = stash.len();
            stash.push(c[0].to_string());
            format!("{PH_OPEN}{idx}{PH_CLOSE}")
        })
        .into_owned();
    (out, stash)
}

fn unmask(line: &str, stash: &[String]) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find(PH_OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start + PH_OPEN.len_utf8()..];
        match after.find(PH_CLOSE) {
            Some(end) => {
                let idx: usize = after[..end].parse().unwrap_or(usize::MAX);
                out.push_str(stash.get(idx).map_or("", String::as_str));
                rest = &after[end + PH_CLOSE.len_utf8()..];
            }
            None => {
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

// ---- dashes -----------------------------------------------------------------

static TABLE_DELIM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)+\|?\s*$").expect("hard-coded regex is valid")
});
static HR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:-{2,}|[—–]+)\s*$").expect("hard-coded regex is valid"));
static ARROW_BOTH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<-{2,}>").expect("hard-coded regex is valid"));
static ARROW_RIGHT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-{2,}>").expect("hard-coded regex is valid"));
static ARROW_LEFT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<-{2,}").expect("hard-coded regex is valid"));
static FLAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(^|[\s(\[{"'])(--[A-Za-z0-9][\w.=/:-]*)"#).expect("hard-coded regex is valid")
});
static LIST_MARKER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\s*)(?:-{2,}|[—–])\s+").expect("hard-coded regex is valid"));
static RANGE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d)\s*(?:[—–]|-{2,})\s*(\d)").expect("hard-coded regex is valid")
});
static DASH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[ \t]*(?:[—–]|-{2,})[ \t]*").expect("hard-coded regex is valid"));
static PUNCT_CLEANUP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*,(\s*,)+").expect("hard-coded regex is valid"));
static DASH_RUN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("-+").expect("hard-coded regex is valid"));
static FORBIDDEN_DASH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[—–]|-{2,}").expect("hard-coded regex is valid"));

fn sanitize_dash_line(line: &str) -> Option<String> {
    if TABLE_DELIM_RE.is_match(line) && line.contains('|') {
        return Some(DASH_RUN_RE.replace_all(line, "-").into_owned());
    }
    if HR_RE.is_match(line) {
        return None;
    }
    let s = ARROW_BOTH_RE.replace_all(line, "↔");
    let s = ARROW_RIGHT_RE.replace_all(&s, "→");
    let s = ARROW_LEFT_RE.replace_all(&s, "←");
    let s = LIST_MARKER_RE.replace_all(&s, "$1- ");
    let s = RANGE_RE.replace_all(&s, "$1-$2");

    // Remaining dashes join clauses: turn them into commas, or drop them at the edges.
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for m in DASH_RE.find_iter(&s) {
        let before = s[..m.start()].trim_end();
        let after = s[m.end()..].trim_start();
        out.push_str(&s[last..m.start()]);
        let prev = before.chars().last();
        let next = after.chars().next();
        let replacement = match (prev, next) {
            (None, _) | (_, None) => "",
            (Some(p), _) if ",;:.!?(".contains(p) => " ",
            (_, Some(n)) if ",;:.!?)".contains(n) => "",
            _ => ", ",
        };
        out.push_str(replacement);
        last = m.end();
    }
    out.push_str(&s[last..]);
    let out = PUNCT_CLEANUP_RE.replace_all(&out, ",").into_owned();
    Some(out.trim_end().to_string())
}

pub fn sanitize_dashes(text: &str) -> String {
    // Bare CLI flags become code spans first, so the dash pass below leaves them intact.
    let flags_wrapped = map_prose_lines(text, CodeBlocks::FencedAndIndented, |l, _| {
        if HR_RE.is_match(l) || TABLE_DELIM_RE.is_match(l) {
            Some(l.to_string())
        } else {
            Some(FLAG_RE.replace_all(l, "$1`$2`").into_owned())
        }
    });
    map_prose_lines(&flags_wrapped, CodeBlocks::FencedAndIndented, |l, _| {
        sanitize_dash_line(l)
    })
}

pub fn dash_violations(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for_each_prose_line(text, CodeBlocks::FencedAndIndented, |line, _| {
        for m in FORBIDDEN_DASH_RE.find_iter(line) {
            let start = line[..m.start()]
                .char_indices()
                .rev()
                .nth(15)
                .map_or(0, |(i, _)| i);
            let end = line[m.end()..]
                .char_indices()
                .nth(15)
                .map_or(line.len(), |(i, _)| m.end() + i);
            found.push(line[start..end].replace([PH_OPEN, PH_CLOSE], ""));
        }
    });
    found
}

// ---- attribution ------------------------------------------------------------

const AI_TERMS: &str = r"(claude|anthropic|chatgpt|openai|copilot|gemini|gpt[- ]?\d*|llms?|large language model|language model|artificial intelligence|an? ai\b|ai\b|a\.i\.)";

static ATTRIBUTION_LINE_RES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)^\s*co-authored-by\s*:",
        r"🤖",
        r"(?i)^\s*(?:[-*_—–]+\s*)?(?:sent|reviewed|signed)?\s*(?:by|from|via)?\s*(?:claude|anthropic)(?:\s+code)?\s*$",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("hard-coded regex is valid"))
    .collect()
});

static ATTRIBUTION_SENTENCE_RES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // "This review was generated with Claude": the subject is the text itself.
        Regex::new(&format!(
            r"(?i)\b(this|the|my|our|these)\s+(review|comment|feedback|analysis|suggestion|summary|response|reply|text|message|note)s?\b[^.!?\n]{{0,30}}\b(generated|written|created|produced|drafted|authored|assisted|powered|composed|made|reviewed)\b[^.!?\n]{{0,25}}\b(with|by|using|via|from|through)\b[^.!?\n]{{0,25}}\b{AI_TERMS}"
        ))
        .expect("hard-coded regex is valid"),
        // Sentence-initial passive signature: "Generated with Claude Code", "Written by an AI".
        // A mid-sentence "errors produced by the Claude CLI" is a legitimate code remark.
        Regex::new(&format!(
            r"(?i)(?:^|[.!?]\s+)[\s*_—–-]*(generated|written|created|produced|drafted|authored|assisted|powered|brought to you|composed|made|reviewed)\s+(with|by|using|via|from|through)\b[^.!?\n]{{0,25}}\b{AI_TERMS}"
        ))
        .expect("hard-coded regex is valid"),
        Regex::new(r"(?i)\bas an? (ai|language model|llm|assistant|automated (reviewer|tool))\b").expect("hard-coded regex is valid"),
        Regex::new(r"(?i)\b(i am|i'm)\s+(claude|an ai|a language model|an assistant|an automated)\b").expect("hard-coded regex is valid"),
        Regex::new(r"(?i)\bai[- ](generated|assisted|powered|authored|written|driven)\b").expect("hard-coded regex is valid"),
        Regex::new(r"(?i)\b(this|the|my)\s+(review|comment|feedback|analysis|suggestion)s?\s+(was|is|were|are|has been|have been)\s+(automated|automatically generated|machine[- ]generated|generated)\b").expect("hard-coded regex is valid"),
    ]
});

static ATTRIBUTION_LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^https?://([a-z0-9-]+\.)*(claude\.(ai|com)|anthropic\.com)\b")
        .expect("hard-coded regex is valid")
});

fn split_sentences(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = line.as_bytes();
    for (i, ch) in line.char_indices() {
        if matches!(ch, '.' | '!' | '?') {
            let next = i + ch.len_utf8();
            if next >= bytes.len() || bytes[next].is_ascii_whitespace() {
                out.push(&line[start..next]);
                start = next;
            }
        }
    }
    if start < line.len() {
        out.push(&line[start..]);
    }
    out
}

/// Masked URLs (not code spans) in `text` that point at an AI vendor.
fn has_attribution_link(text: &str, stash: &[String]) -> bool {
    let mut rest = text;
    while let Some(start) = rest.find(PH_OPEN) {
        let after = &rest[start + PH_OPEN.len_utf8()..];
        let Some(end) = after.find(PH_CLOSE) else {
            break;
        };
        if let Some(entry) = after[..end]
            .parse::<usize>()
            .ok()
            .and_then(|i| stash.get(i))
            && ATTRIBUTION_LINK_RE.is_match(entry)
        {
            return true;
        }
        rest = &after[end + PH_CLOSE.len_utf8()..];
    }
    false
}

fn attribution_hit(text: &str, stash: &[String]) -> bool {
    ATTRIBUTION_SENTENCE_RES.iter().any(|r| r.is_match(text)) || has_attribution_link(text, stash)
}

fn strip_attribution_line(line: &str, stash: &[String]) -> Option<String> {
    if ATTRIBUTION_LINE_RES.iter().any(|r| r.is_match(line)) {
        return None;
    }
    if !attribution_hit(line, stash) {
        return Some(line.to_string());
    }
    let kept: String = split_sentences(line)
        .into_iter()
        .filter(|s| !attribution_hit(s, stash))
        .collect();
    let joined = kept.trim().to_string();
    if joined.is_empty() {
        None
    } else {
        let indent = &line[..line.len() - line.trim_start().len()];
        Some(format!("{indent}{joined}"))
    }
}

pub fn strip_attribution(text: &str) -> String {
    map_prose_lines(text, CodeBlocks::Fenced, strip_attribution_line)
}

pub fn attribution_violations(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for_each_prose_line(text, CodeBlocks::Fenced, |line, stash| {
        if ATTRIBUTION_LINE_RES.iter().any(|r| r.is_match(line)) || attribution_hit(line, stash) {
            found.push(unmask(line, stash).trim().to_string());
        }
    });
    found
}

// ---- public API -------------------------------------------------------------

static BLANK_RUN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\n{3,}").expect("hard-coded regex is valid"));

/// Whether AI attribution is stripped from comments (`forbid_ai_attribution`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    Allowed,
    Forbidden,
}

impl Attribution {
    pub fn from_settings(settings: &EffectiveSettings) -> Self {
        if settings.forbid_ai_attribution.value {
            Self::Forbidden
        } else {
            Self::Allowed
        }
    }
}

/// Sanitize text that will be posted.
pub fn sanitize_comment(text: &str, attribution: Attribution) -> String {
    let text = text.replace("\r\n", "\n");
    let text = match attribution {
        Attribution::Forbidden => strip_attribution(&text),
        Attribution::Allowed => text,
    };
    let text = sanitize_dashes(&text);
    BLANK_RUN_RE.replace_all(&text, "\n\n").trim().to_string()
}

#[derive(Debug, Clone, Default, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CommentCheck {
    pub dash_violations: Vec<String>,
    pub attribution_violations: Vec<String>,
    /// What the text becomes after sanitizing.
    pub sanitized: String,
}

pub fn check_comment(text: &str, attribution: Attribution) -> CommentCheck {
    CommentCheck {
        dash_violations: dash_violations(text),
        attribution_violations: match attribution {
            Attribution::Forbidden => attribution_violations(text),
            Attribution::Allowed => Vec::new(),
        },
        sanitized: sanitize_comment(text, attribution),
    }
}

/// Final gate before posting: sanitize, append the optional footer, and refuse if anything
/// forbidden survives.
pub fn finalize_body(text: &str, settings: &EffectiveSettings) -> AppResult<String> {
    let attribution = Attribution::from_settings(settings);
    let mut body = sanitize_comment(text, attribution);
    let footer = settings.attribution_footer.value.trim();
    if attribution == Attribution::Allowed && !footer.is_empty() && !body.is_empty() {
        body = format!("{body}\n\n{}", sanitize_dashes(footer));
    }
    let dashes = dash_violations(&body);
    if !dashes.is_empty() {
        return Err(AppError::invalid(format!(
            "refusing to post: double dashes remain in: {}",
            dashes.join(" | ")
        )));
    }
    if attribution == Attribution::Forbidden {
        let remaining = attribution_violations(&body);
        if !remaining.is_empty() {
            return Err(AppError::invalid(format!(
                "refusing to post: AI attribution remains in: {}",
                remaining.join(" | ")
            )));
        }
    }
    Ok(body)
}

/// Sorts by severity, then PR-wide findings last, then file and line. Also renumbers
/// `position` to match, which is the order the UI shows.
pub fn sort_findings(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| a.file.is_none().cmp(&b.file.is_none()))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line_start.cmp(&b.line_start))
    });
    for (i, f) in findings.iter_mut().enumerate() {
        f.position = u32::try_from(i).unwrap_or(u32::MAX);
    }
}

pub struct Processed {
    pub verdict: Verdict,
    pub summary: String,
    pub findings: Vec<Finding>,
    pub critical_files: Vec<CriticalFile>,
}

pub const SCOPE_CATEGORY: &str = "scope";

/// Turn raw model output into stored findings. `has_tickets`: whether ticket context was given
/// to the reviewer.
pub fn process(
    raw: RawReview,
    review_id: &str,
    settings: &EffectiveSettings,
    diff: &DiffIndex,
    has_tickets: bool,
) -> Processed {
    let attribution = Attribution::from_settings(settings);
    let min_selected = settings.default_selected_min_severity.value;
    let scope_allowed = has_tickets && settings.ticket_scope_check.value;

    let mut findings: Vec<Finding> = raw
        .findings
        .into_iter()
        .filter_map(|f| to_finding(f, review_id, attribution, diff))
        .filter(|f| !(settings.skip_nits.value && f.severity == Severity::Nit))
        .filter(|f| scope_allowed || f.category != SCOPE_CATEGORY)
        .map(|mut f| {
            if f.category == SCOPE_CATEGORY && f.severity < Severity::Medium {
                f.severity = Severity::Medium;
            }
            f
        })
        .collect();
    sort_findings(&mut findings);
    for f in &mut findings {
        f.selected = f.severity <= min_selected && !f.proposed_comment.is_empty();
    }

    let mut verdict = Verdict::parse(raw.verdict.trim()).unwrap_or(Verdict::Comment);
    // Never let an approve verdict through with blocking findings.
    if verdict == Verdict::Approve && findings.iter().any(|f| f.severity <= Severity::Medium) {
        verdict = if findings.iter().any(|f| f.severity <= Severity::High) {
            Verdict::RequestChanges
        } else {
            Verdict::Comment
        };
    }

    Processed {
        verdict,
        summary: sanitize_comment(&raw.summary, attribution),
        findings,
        critical_files: critical_files(raw.critical_files, diff),
    }
}

pub const MAX_CRITICAL_FILES: usize = 5;
const MAX_FOCUS_RANGES: usize = 10;
const MAX_REASON_CHARS: usize = 1000;
/// Larger line numbers are model noise; no reviewable file is this long.
const MAX_LINE: i64 = 1_000_000;

/// The files the reviewer recommends reading by hand. Only paths changed in the PR survive
/// (model output is untrusted, and the path is later used to read the file from git),
/// duplicates are dropped, and the list is capped. The reason is shown to the user only and
/// never posted, so the comment rules don't apply to it.
fn critical_files(raw: Vec<RawCriticalFile>, diff: &DiffIndex) -> Vec<CriticalFile> {
    let mut out: Vec<CriticalFile> = Vec::new();
    for f in raw {
        let wanted = normalize_path(&f.path, diff);
        let Some(file) = diff.file(&wanted) else {
            continue;
        };
        let path = if file.new_path.is_empty() {
            file.old_path.clone()
        } else {
            file.new_path.clone()
        };
        if out.iter().any(|c| c.path == path) {
            continue;
        }
        let mut focus_lines: Vec<LineRange> = f
            .focus_lines
            .iter()
            .filter_map(|r| {
                let start = u32::try_from(r.start.clamp(0, MAX_LINE)).ok()?;
                let end = u32::try_from(r.end.max(r.start).clamp(0, MAX_LINE)).ok()?;
                (start > 0).then_some(LineRange { start, end })
            })
            .collect();
        focus_lines.sort_by_key(|r| (r.start, r.end));
        focus_lines.dedup();
        focus_lines.truncate(MAX_FOCUS_RANGES);
        out.push(CriticalFile {
            path,
            reason: f.reason.trim().chars().take(MAX_REASON_CHARS).collect(),
            focus_lines,
            note: None,
        });
        if out.len() == MAX_CRITICAL_FILES {
            break;
        }
    }
    out
}

/// The model sometimes copies git's `b/` prefix or adds `./`. Strip at most one of each, and
/// only when the path as given isn't in the diff, so a real top-level `b/` folder still works.
fn normalize_path(p: &str, diff: &DiffIndex) -> String {
    let p = p.trim();
    let dot = p.strip_prefix("./").unwrap_or(p);
    let candidates = [p, dot, dot.strip_prefix("b/").unwrap_or(dot)];
    candidates
        .iter()
        .find(|c| diff.file(c).is_some())
        .unwrap_or(&candidates[2])
        .to_string()
}

fn to_finding(
    f: RawFinding,
    review_id: &str,
    attribution: Attribution,
    diff: &DiffIndex,
) -> Option<Finding> {
    let severity = Severity::parse(&f.severity).unwrap_or(Severity::Low);
    let side = match f.side.as_deref() {
        Some("left") => Side::Left,
        _ => Side::Right,
    };
    let file = f
        .file
        .map(|p| normalize_path(&p, diff))
        .filter(|p| !p.is_empty());
    let line_start = f.line_start.filter(|l| *l > 0);
    let line_end = f.line_end.filter(|l| *l > 0 && Some(*l) > line_start);
    let anchor_line = line_end.or(line_start);

    let anchor = match (&file, anchor_line) {
        (Some(path), Some(line)) => diff.locate(path, line, side),
        _ => None,
    };
    let excerpt = match (&file, line_start.or(anchor_line)) {
        (Some(path), Some(line)) => anchor
            .as_ref()
            .and_then(|a| diff.excerpt(path, line, a.side, 3)),
        _ => None,
    };

    let proposed_comment = sanitize_comment(&f.proposed_comment, attribution);
    let title = sanitize_comment(&f.title, attribution);
    if title.is_empty() && proposed_comment.is_empty() {
        return None;
    }
    Some(Finding {
        id: new_id(),
        review_id: review_id.to_string(),
        position: 0,
        severity,
        category: f
            .category
            .map(|c| c.trim().to_ascii_lowercase())
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "general".into()),
        title,
        file,
        line_start,
        line_end,
        side: anchor.as_ref().map_or(side, |a| a.side),
        explanation: sanitize_comment(&f.explanation, attribution),
        proposed_comment,
        suggestion: f.suggestion.filter(|s| !s.trim().is_empty()),
        in_diff: anchor.is_some(),
        diff_excerpt: excerpt,
        selected: false,
        posted_at: None,
        posted_ref: None,
        source: FindingSource::Reviewer,
    })
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;
    use crate::settings::{SettingsOverrides, resolve};

    #[test]
    fn replaces_dashes_in_prose() {
        assert_eq!(
            sanitize_dashes("This is wrong — it leaks memory."),
            "This is wrong, it leaks memory."
        );
        assert_eq!(
            sanitize_dashes("This is wrong -- it leaks memory."),
            "This is wrong, it leaks memory."
        );
        assert_eq!(sanitize_dashes("word—word"), "word, word");
        assert_eq!(sanitize_dashes("pages 3–5 and 7--9"), "pages 3-5 and 7-9");
        assert_eq!(sanitize_dashes("ends with a dash —"), "ends with a dash");
        assert_eq!(sanitize_dashes("Note: — careful"), "Note: careful");
        assert_eq!(sanitize_dashes("a --> b <-- c"), "a → b ← c");
        assert_eq!(sanitize_dashes("-- first\n— second"), "- first\n- second");
        assert_eq!(sanitize_dashes("Intro\n---\nBody"), "Intro\nBody");
        assert_eq!(sanitize_dashes("a – b — c -- d"), "a, b, c, d");
    }

    #[test]
    fn leaves_code_and_urls_alone() {
        let text = "Use `git push --force-with-lease` instead.\n```sh\ncargo test -- --nocapture\n# — comment\n```\nSee https://x.dev/a--b for details.";
        assert_eq!(sanitize_dashes(text), text);
        assert!(dash_violations(text).is_empty());
    }

    #[test]
    fn wraps_bare_cli_flags_in_code() {
        assert_eq!(
            sanitize_dashes("Pass --no-verify to skip hooks."),
            "Pass `--no-verify` to skip hooks."
        );
        assert!(dash_violations(&sanitize_dashes("(--dry-run) works")).is_empty());
    }

    #[test]
    fn shrinks_table_delimiters() {
        let table = "| a | b |\n|---|:---:|\n| 1 | 2 |";
        assert_eq!(sanitize_dashes(table), "| a | b |\n|-|:-:|\n| 1 | 2 |");
    }

    #[test]
    fn detects_violations_outside_code_only() {
        let v = dash_violations("ok `a--b` but this — not");
        assert_eq!(v.len(), 1);
        assert!(v[0].contains('—'));
    }

    #[test]
    fn strips_attribution() {
        let text = "Please handle the error here.\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nCo-Authored-By: Claude <noreply@anthropic.com>";
        assert_eq!(
            sanitize_comment(text, Attribution::Forbidden),
            "Please handle the error here."
        );

        let text =
            "This leaks a handle. This review was generated by an AI assistant. Close it in Drop.";
        assert_eq!(
            sanitize_comment(text, Attribution::Forbidden),
            "This leaks a handle. Close it in Drop."
        );

        let text = "As an AI, I think this is fine. The loop is O(n^2).";
        assert_eq!(
            sanitize_comment(text, Attribution::Forbidden),
            "The loop is O(n^2)."
        );
    }

    #[test]
    fn keeps_legitimate_mentions_in_code() {
        let text = "Rename `ClaudeClient::send` to match the other clients.";
        assert_eq!(sanitize_comment(text, Attribution::Forbidden), text);
        assert!(attribution_violations(text).is_empty());
    }

    #[test]
    fn attribution_allowed_keeps_text() {
        let text = "Generated with Claude.";
        assert_eq!(sanitize_comment(text, Attribution::Allowed), text);
    }

    fn settings(over: SettingsOverrides) -> EffectiveSettings {
        resolve(&SettingsOverrides::default(), &over, None)
    }

    #[test]
    fn finalize_appends_footer_only_when_allowed() {
        let allowed = settings(SettingsOverrides {
            forbid_ai_attribution: Some(false),
            attribution_footer: Some("Reviewed with Claude".into()),
            ..Default::default()
        });
        assert_eq!(
            finalize_body("Looks good", &allowed).unwrap(),
            "Looks good\n\nReviewed with Claude"
        );

        let forbidden = settings(SettingsOverrides {
            attribution_footer: Some("Reviewed with Claude".into()),
            ..Default::default()
        });
        assert_eq!(
            finalize_body("Looks good — ship it", &forbidden).unwrap(),
            "Looks good, ship it"
        );
    }

    fn raw(sev: &str, file: &str, line: u32, comment: &str) -> RawFinding {
        RawFinding {
            severity: sev.into(),
            category: Some("bug".into()),
            title: format!("{sev} issue"),
            file: Some(file.into()),
            line_start: Some(line),
            line_end: None,
            side: None,
            explanation: "why".into(),
            proposed_comment: comment.into(),
            suggestion: None,
        }
    }

    const DIFF: &str =
        "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,3 @@\n x\n+y\n z\n";

    #[test]
    fn critical_files_must_be_in_the_diff_and_are_capped() {
        let diff = DiffIndex::parse(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1,2 @@\n x\n+y\n\
             diff --git a/old.rs b/new.rs\nrename from old.rs\nrename to new.rs\n",
        );
        let file = |path: &str, ranges: &[(i64, i64)]| RawCriticalFile {
            path: path.into(),
            reason: format!("  look at {path}  "),
            focus_lines: ranges
                .iter()
                .map(|&(start, end)| crate::review::schema::RawLineRange { start, end })
                .collect(),
        };
        let out = critical_files(
            vec![
                file("./a.rs", &[(9, 3), (0, 4), (2, 5), (2, 5), (-1, 2)]),
                file("../../etc/passwd", &[]),
                file("not-changed.rs", &[]),
                file("b/a.rs", &[]),
                file("old.rs", &[(1, 99_999_999)]),
            ],
            &diff,
        );
        let paths: Vec<_> = out.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["a.rs", "new.rs"]);
        assert_eq!(out[0].reason, "look at ./a.rs");
        assert_eq!(
            out[0].focus_lines,
            vec![
                LineRange { start: 2, end: 5 },
                LineRange { start: 9, end: 9 }
            ]
        );
        assert_eq!(
            out[1].focus_lines,
            vec![LineRange {
                start: 1,
                end: u32::try_from(MAX_LINE).unwrap()
            }]
        );

        // A real top-level `b/` folder is not mistaken for git's prefix.
        let real_b = DiffIndex::parse(
            "diff --git a/b/auth.rs b/b/auth.rs\n--- a/b/auth.rs\n+++ b/b/auth.rs\n@@ -1 +1 @@\n-a\n+b\n",
        );
        let found: Vec<_> = critical_files(
            vec![file("b/auth.rs", &[]), file("b/b/auth.rs", &[])],
            &real_b,
        )
        .into_iter()
        .map(|f| f.path)
        .collect();
        assert_eq!(found, ["b/auth.rs"]);
        assert_eq!(normalize_path("./b/b/auth.rs", &real_b), "b/auth.rs");
        assert_eq!(normalize_path("b/x.rs", &real_b), "x.rs");

        let mut wide = String::new();
        for i in 0..8 {
            let _ = write!(
                wide,
                "diff --git a/f{i}.rs b/f{i}.rs\n--- a/f{i}.rs\n+++ b/f{i}.rs\n@@ -1 +1 @@\n-a\n+b\n"
            );
        }
        let many: Vec<_> = (0..8).map(|i| file(&format!("f{i}.rs"), &[])).collect();
        assert_eq!(
            critical_files(many, &DiffIndex::parse(&wide)).len(),
            MAX_CRITICAL_FILES
        );
        let long = RawCriticalFile {
            reason: "x".repeat(5000),
            ..file("a.rs", &[])
        };
        assert_eq!(
            critical_files(vec![long], &diff)[0].reason.len(),
            MAX_REASON_CHARS
        );
    }

    /// An approve verdict with one finding per severity, reviewed with `skip_nits` on.
    fn process_mixed_review() -> Processed {
        let diff = DiffIndex::parse(DIFF);
        let raw_review = RawReview {
            verdict: "approve".into(),
            summary: "Summary — fine".into(),
            critical_files: Vec::new(),
            findings: vec![
                raw("nit", "a.rs", 2, "nit"),
                raw("low", "b.rs", 9, "low"),
                raw("critical", "a.rs", 2, "Fix this -- now"),
                raw("medium", "a.rs", 50, "medium"),
            ],
        };
        let s = settings(SettingsOverrides {
            skip_nits: Some(true),
            ..Default::default()
        });
        process(raw_review, "r1", &s, &diff, false)
    }

    #[test]
    fn process_drops_nits_and_sorts_by_severity() {
        let p = process_mixed_review();
        let sev: Vec<_> = p.findings.iter().map(|f| f.severity).collect();
        assert_eq!(
            sev,
            vec![Severity::Critical, Severity::Medium, Severity::Low]
        );
    }

    #[test]
    fn process_numbers_positions_in_sorted_order() {
        let p = process_mixed_review();
        assert_eq!(
            p.findings.iter().map(|f| f.position).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn process_sanitizes_dashes_in_comments_and_summary() {
        let p = process_mixed_review();
        assert_eq!(
            (p.findings[0].proposed_comment.as_str(), p.summary.as_str()),
            ("Fix this, now", "Summary, fine")
        );
    }

    #[test]
    fn process_anchors_findings_on_diff_lines_with_an_excerpt() {
        let p = process_mixed_review();
        assert!(p.findings[0].in_diff && p.findings[0].diff_excerpt.is_some());
    }

    #[test]
    fn process_marks_findings_outside_the_diff() {
        let p = process_mixed_review();
        // Line 50 of a.rs is not part of the diff.
        assert!(!p.findings[1].in_diff);
    }

    #[test]
    fn process_preselects_findings_from_the_default_minimum_severity() {
        let p = process_mixed_review();
        let selected: Vec<_> = p.findings.iter().map(|f| f.selected).collect();
        assert_eq!(selected, vec![true, true, false]);
    }

    #[test]
    fn approve_with_a_critical_finding_is_downgraded() {
        assert_eq!(process_mixed_review().verdict, Verdict::RequestChanges);
    }

    /// An approve verdict with a single nit, reviewed with default settings.
    fn process_nit_only_review() -> Processed {
        let diff = DiffIndex::parse(DIFF);
        let raw_review = RawReview {
            verdict: "approve".into(),
            summary: "LGTM".into(),
            critical_files: Vec::new(),
            findings: vec![raw("nit", "a.rs", 2, "tiny")],
        };
        process(
            raw_review,
            "r1",
            &settings(SettingsOverrides::default()),
            &diff,
            false,
        )
    }

    #[test]
    fn process_keeps_nits_when_skip_nits_is_off() {
        assert_eq!(process_nit_only_review().findings.len(), 1);
    }

    #[test]
    fn approve_with_only_nits_stays_approve() {
        assert_eq!(process_nit_only_review().verdict, Verdict::Approve);
    }

    #[test]
    fn scope_findings_are_capped_and_need_a_ticket() {
        let diff = DiffIndex::parse(DIFF);
        let review = || RawReview {
            verdict: "request_changes".into(),
            summary: "s".into(),
            critical_files: Vec::new(),
            findings: vec![
                RawFinding {
                    category: Some(" Scope".into()),
                    file: None,
                    line_start: None,
                    ..raw("critical", "", 0, "The ticket asks for an expiry")
                },
                raw("low", "a.rs", 2, "low"),
            ],
        };
        let on = settings(SettingsOverrides::default());
        let p = process(review(), "r1", &on, &diff, true);
        assert_eq!(p.findings[0].category, "scope");
        assert_eq!(p.findings[0].severity, Severity::Medium);

        // No ticket loaded, or the check is off: scope findings are dropped.
        let p = process(review(), "r1", &on, &diff, false);
        assert_eq!(p.findings.len(), 1);
        let off = settings(SettingsOverrides {
            ticket_scope_check: Some(false),
            ..Default::default()
        });
        let p = process(review(), "r1", &off, &diff, true);
        assert!(p.findings.iter().all(|f| f.category != "scope"));
    }

    #[test]
    fn code_remarks_about_ai_tools_are_not_attribution() {
        for text in [
            "Errors produced by the Claude CLI are swallowed here. Return them.",
            "The token is created by the OpenAI client but never closed.",
        ] {
            assert_eq!(sanitize_comment(text, Attribution::Forbidden), text);
            assert!(attribution_violations(text).is_empty(), "{text}");
        }
        assert_eq!(
            sanitize_comment(
                "Looks good. Generated with Claude Code.",
                Attribution::Forbidden
            ),
            "Looks good."
        );
        assert_eq!(
            sanitize_comment(
                "Fine. This review was written by an AI.",
                Attribution::Forbidden
            ),
            "Fine."
        );
    }

    #[test]
    fn em_dash_signatures_are_stripped_not_blocked() {
        let out = sanitize_comment("Thanks!\n\n— Claude", Attribution::Forbidden);
        assert_eq!(out, "Thanks!");
        assert!(
            check_comment(&out, Attribution::Forbidden)
                .attribution_violations
                .is_empty()
        );
    }

    #[test]
    fn indented_code_blocks_keep_their_dashes() {
        let text = "Run the tests with output:\n\n    cargo test -- --nocapture\n\nThen check -- carefully.";
        let out = sanitize_comment(text, Attribution::Forbidden);
        assert!(out.contains("    cargo test -- --nocapture"), "{out}");
        assert!(!out.contains("check --"), "{out}");
        assert!(dash_violations(&out).is_empty());
        // Indentation is no loophole for attribution.
        assert!(!attribution_violations("Done.\n\n    Generated with Claude Code").is_empty());
    }
}
