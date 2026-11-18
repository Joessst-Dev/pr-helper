//! Deterministic enforcement of the comment rules. The model is told about every rule, but
//! nothing it returns is trusted: this runs on model output and again on user-edited text
//! right before anything is posted.
//!
//! Rules:
//! - No `--`, em dash or en dash in prose. Inline code, fenced code and URLs are left untouched.
//! - Optional: no AI attribution of any kind.
//! - Optional: drop nits.
//! - Findings sorted by severity, then file and line.

use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::db::models::{Finding, Severity, Side, Verdict};
use crate::db::new_id;
use crate::error::{AppError, AppResult};
use crate::git::diff::DiffIndex;
use crate::review::schema::{RawFinding, RawReview};
use crate::settings::EffectiveSettings;

// ---- markdown segmentation --------------------------------------------------

const PH_OPEN: char = '\u{E000}';
const PH_CLOSE: char = '\u{E001}';

static URL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"https?://[^\s<>()\[\]]+").unwrap());

/// Apply `f` to every prose line. Fenced code blocks are skipped entirely; inline code spans and
/// URLs inside prose lines are swapped for placeholders while `f` runs. `f` may drop a line.
fn map_prose_lines(text: &str, mut f: impl FnMut(&str, &[String]) -> Option<String>) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in text.split('\n') {
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
fn for_each_prose_line(text: &str, mut f: impl FnMut(&str, &[String])) {
    map_prose_lines(text, |l, stash| {
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
                out.push_str(stash.get(idx).map(String::as_str).unwrap_or(""));
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

static TABLE_DELIM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)+\|?\s*$").unwrap());
static HR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(?:-{2,}|[—–]+)\s*$").unwrap());
static ARROW_BOTH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<-{2,}>").unwrap());
static ARROW_RIGHT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-{2,}>").unwrap());
static ARROW_LEFT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<-{2,}").unwrap());
static FLAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(^|[\s(\[{"'])(--[A-Za-z0-9][\w.=/:-]*)"#).unwrap());
static LIST_MARKER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\s*)(?:-{2,}|[—–])\s+").unwrap());
static RANGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d)\s*(?:[—–]|-{2,})\s*(\d)").unwrap());
static DASH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[ \t]*(?:[—–]|-{2,})[ \t]*").unwrap());
static PUNCT_CLEANUP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*,(\s*,)+").unwrap());
static FORBIDDEN_DASH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[—–]|-{2,}").unwrap());

fn sanitize_dash_line(line: &str) -> Option<String> {
    if TABLE_DELIM_RE.is_match(line) && line.contains('|') {
        return Some(
            Regex::new("-+")
                .unwrap()
                .replace_all(line, "-")
                .into_owned(),
        );
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
    let flags_wrapped = map_prose_lines(text, |l, _| {
        if HR_RE.is_match(l) || TABLE_DELIM_RE.is_match(l) {
            Some(l.to_string())
        } else {
            Some(FLAG_RE.replace_all(l, "$1`$2`").into_owned())
        }
    });
    map_prose_lines(&flags_wrapped, |l, _| sanitize_dash_line(l))
}

pub fn dash_violations(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for_each_prose_line(text, |line, _| {
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
        r"(?i)^\s*(?:[-*_]\s*)?(?:sent|reviewed|signed)?\s*(?:by|from|via)?\s*(?:claude|anthropic)(?:\s+code)?\s*$",
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap())
    .collect()
});

static ATTRIBUTION_SENTENCE_RES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(&format!(
            r"(?i)\b(generated|written|created|produced|reviewed|drafted|authored|assisted|powered|brought to you|composed|made)\b[^.!?\n]{{0,40}}\b(with|by|using|via|from|through)\b[^.!?\n]{{0,40}}\b{AI_TERMS}"
        ))
        .unwrap(),
        Regex::new(r"(?i)\bas an? (ai|language model|llm|assistant|automated (reviewer|tool))\b").unwrap(),
        Regex::new(r"(?i)\b(i am|i'm)\s+(claude|an ai|a language model|an assistant|an automated)\b").unwrap(),
        Regex::new(r"(?i)\bai[- ](generated|assisted|powered|authored|written|driven)\b").unwrap(),
        Regex::new(r"(?i)\b(this|the|my)\s+(review|comment|feedback|analysis|suggestion)s?\s+(was|is|were|are|has been|have been)\s+(automated|automatically generated|machine[- ]generated|generated)\b").unwrap(),
    ]
});

static ATTRIBUTION_LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^https?://([a-z0-9-]+\.)*(claude\.(ai|com)|anthropic\.com)\b").unwrap()
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
    let kept: Vec<&str> = split_sentences(line)
        .into_iter()
        .filter(|s| !attribution_hit(s, stash))
        .collect();
    let joined = kept.concat().trim().to_string();
    if joined.is_empty() {
        None
    } else {
        let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
        Some(format!("{indent}{joined}"))
    }
}

pub fn strip_attribution(text: &str) -> String {
    map_prose_lines(text, strip_attribution_line)
}

pub fn attribution_violations(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for_each_prose_line(text, |line, stash| {
        if ATTRIBUTION_LINE_RES.iter().any(|r| r.is_match(line)) || attribution_hit(line, stash) {
            found.push(unmask(line, stash).trim().to_string());
        }
    });
    found
}

// ---- public API -------------------------------------------------------------

static BLANK_RUN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").unwrap());

/// Sanitize text that will be posted.
pub fn sanitize_comment(text: &str, forbid_attribution: bool) -> String {
    let text = text.replace("\r\n", "\n");
    let text = if forbid_attribution {
        strip_attribution(&text)
    } else {
        text
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

pub fn check_comment(text: &str, forbid_attribution: bool) -> CommentCheck {
    CommentCheck {
        dash_violations: dash_violations(text),
        attribution_violations: if forbid_attribution {
            attribution_violations(text)
        } else {
            Vec::new()
        },
        sanitized: sanitize_comment(text, forbid_attribution),
    }
}

/// Final gate before posting: sanitize, append the optional footer, and refuse if anything
/// forbidden survives.
pub fn finalize_body(text: &str, settings: &EffectiveSettings) -> AppResult<String> {
    let forbid = settings.forbid_ai_attribution.value;
    let mut body = sanitize_comment(text, forbid);
    let footer = settings.attribution_footer.value.trim();
    if !forbid && !footer.is_empty() && !body.is_empty() {
        body = format!("{body}\n\n{}", sanitize_dashes(footer));
    }
    let dashes = dash_violations(&body);
    if !dashes.is_empty() {
        return Err(AppError::invalid(format!(
            "refusing to post: double dashes remain in: {}",
            dashes.join(" | ")
        )));
    }
    if forbid {
        let attribution = attribution_violations(&body);
        if !attribution.is_empty() {
            return Err(AppError::invalid(format!(
                "refusing to post: AI attribution remains in: {}",
                attribution.join(" | ")
            )));
        }
    }
    Ok(body)
}

pub fn sort_findings(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        a.severity
            .rank()
            .cmp(&b.severity.rank())
            .then_with(|| a.file.is_none().cmp(&b.file.is_none()))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line_start.cmp(&b.line_start))
    });
    for (i, f) in findings.iter_mut().enumerate() {
        f.position = i as u32;
    }
}

pub struct Processed {
    pub verdict: Verdict,
    pub summary: String,
    pub findings: Vec<Finding>,
}

/// Turn raw model output into stored findings.
pub fn process(
    raw: RawReview,
    review_id: &str,
    settings: &EffectiveSettings,
    diff: &DiffIndex,
) -> Processed {
    let forbid = settings.forbid_ai_attribution.value;
    let min_selected = settings.default_selected_min_severity.value;

    let mut findings: Vec<Finding> = raw
        .findings
        .into_iter()
        .filter_map(|f| to_finding(f, review_id, forbid, diff))
        .filter(|f| !(settings.skip_nits.value && f.severity == Severity::Nit))
        .collect();
    sort_findings(&mut findings);
    for f in &mut findings {
        f.selected = f.severity.rank() <= min_selected.rank() && !f.proposed_comment.is_empty();
    }

    let mut verdict = Verdict::parse(raw.verdict.trim()).unwrap_or(Verdict::Comment);
    // Never let an approve verdict through with blocking findings.
    if verdict == Verdict::Approve
        && findings
            .iter()
            .any(|f| f.severity.rank() <= Severity::Medium.rank())
    {
        verdict = if findings
            .iter()
            .any(|f| f.severity.rank() <= Severity::High.rank())
        {
            Verdict::RequestChanges
        } else {
            Verdict::Comment
        };
    }

    Processed {
        verdict,
        summary: sanitize_comment(&raw.summary, forbid),
        findings,
    }
}

fn to_finding(f: RawFinding, review_id: &str, forbid: bool, diff: &DiffIndex) -> Option<Finding> {
    let severity = Severity::parse(&f.severity).unwrap_or(Severity::Low);
    let side = match f.side.as_deref() {
        Some("left") => Side::Left,
        _ => Side::Right,
    };
    let file = f
        .file
        .map(|p| {
            p.trim()
                .trim_start_matches("./")
                .trim_start_matches("b/")
                .to_string()
        })
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

    let proposed_comment = sanitize_comment(&f.proposed_comment, forbid);
    let title = sanitize_comment(&f.title, forbid);
    if title.is_empty() && proposed_comment.is_empty() {
        return None;
    }
    Some(Finding {
        id: new_id(),
        review_id: review_id.to_string(),
        position: 0,
        severity,
        category: f.category.unwrap_or_else(|| "general".into()),
        title,
        file,
        line_start,
        line_end,
        side: anchor.as_ref().map_or(side, |a| a.side),
        explanation: sanitize_comment(&f.explanation, forbid),
        proposed_comment,
        suggestion: f.suggestion.filter(|s| !s.trim().is_empty()),
        in_diff: anchor.is_some(),
        diff_excerpt: excerpt,
        selected: false,
        posted_at: None,
        posted_ref: None,
    })
}

#[cfg(test)]
mod tests {
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
            sanitize_comment(text, true),
            "Please handle the error here."
        );

        let text =
            "This leaks a handle. This review was generated by an AI assistant. Close it in Drop.";
        assert_eq!(
            sanitize_comment(text, true),
            "This leaks a handle. Close it in Drop."
        );

        let text = "As an AI, I think this is fine. The loop is O(n^2).";
        assert_eq!(sanitize_comment(text, true), "The loop is O(n^2).");
    }

    #[test]
    fn keeps_legitimate_mentions_in_code() {
        let text = "Rename `ClaudeClient::send` to match the other clients.";
        assert_eq!(sanitize_comment(text, true), text);
        assert!(attribution_violations(text).is_empty());
    }

    #[test]
    fn attribution_allowed_keeps_text() {
        let text = "Generated with Claude.";
        assert_eq!(sanitize_comment(text, false), text);
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
    fn process_sorts_filters_and_anchors() {
        let diff = DiffIndex::parse(DIFF);
        let raw_review = RawReview {
            verdict: "approve".into(),
            summary: "Summary — fine".into(),
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
        let p = process(raw_review, "r1", &s, &diff);
        let sev: Vec<_> = p.findings.iter().map(|f| f.severity).collect();
        assert_eq!(
            sev,
            vec![Severity::Critical, Severity::Medium, Severity::Low]
        );
        assert_eq!(p.findings[0].proposed_comment, "Fix this, now");
        assert!(p.findings[0].in_diff);
        assert!(p.findings[0].diff_excerpt.is_some());
        assert!(!p.findings[1].in_diff); // line 50 is not in the diff
        assert!(p.findings[0].selected && p.findings[1].selected && !p.findings[2].selected);
        assert_eq!(
            p.findings.iter().map(|f| f.position).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        // Approve with a critical finding is downgraded.
        assert_eq!(p.verdict, Verdict::RequestChanges);
        assert_eq!(p.summary, "Summary, fine");
    }

    #[test]
    fn process_keeps_nits_when_allowed_and_respects_clean_approve() {
        let diff = DiffIndex::parse(DIFF);
        let raw_review = RawReview {
            verdict: "approve".into(),
            summary: "LGTM".into(),
            findings: vec![raw("nit", "a.rs", 2, "tiny")],
        };
        let p = process(
            raw_review,
            "r1",
            &settings(SettingsOverrides::default()),
            &diff,
        );
        assert_eq!(p.findings.len(), 1);
        assert_eq!(p.verdict, Verdict::Approve);
    }
}
