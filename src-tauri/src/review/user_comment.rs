//! Comments the user writes on the critical files, and the optional AI check of such a comment.
//!
//! A user comment is stored as a finding with `source = user`, so selecting, editing and posting
//! (including `finalize_body`) work exactly as for the reviewer's findings.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::db::models::{
    CriticalFileContent, DiffLineKind, FileDiffLine, FileNote, Finding, FindingSource, Review,
    ReviewStatus, Severity, Side,
};
use crate::db::new_id;
use crate::env;
use crate::error::{AppError, AppResult};
use crate::review::postprocess::{Attribution, sanitize_comment, sanitize_dashes};
use crate::review::prompt::{comment_check_prompt, comment_check_system_prompt};
use crate::review::runner::{self, RunRequest, ToolPolicy};
use crate::review::schema::{RawCommentAdvice, comment_check_schema};
use crate::service::Service;
use crate::settings::EffectiveSettings;

pub const USER_CATEGORY: &str = "comment";
const MAX_BODY_CHARS: usize = 20_000;
const MAX_TITLE_CHARS: usize = 80;
const CHECK_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// A comment the user wrote on lines of a critical file.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UserCommentInput {
    pub review_id: String,
    pub file: String,
    pub line_start: u32,
    /// Last line of a range; `None` for a single line.
    pub line_end: Option<u32>,
    pub side: Side,
    pub severity: Severity,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CommentAssessment {
    Valid,
    PartlyValid,
    NotValid,
}

/// What the AI thinks of a user comment. Advisory only: it is never applied or posted by itself.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CommentAdvice {
    pub assessment: CommentAssessment,
    pub feedback: String,
    pub suggested_comment: String,
    pub cost_usd: Option<f64>,
}

/// The lines a comment is anchored to, checked against the stored file and diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Anchor {
    start: u32,
    end: u32,
    side: Side,
    in_diff: bool,
}

fn diff_number(d: &FileDiffLine, side: Side) -> Option<u32> {
    match side {
        Side::Right => d.new,
        Side::Left => d.old,
    }
}

/// Checks the lines against the stored file and diff and tells whether they can be commented
/// on inline (`in_diff`) or end up in the review body.
fn validate_anchor(input: &UserCommentInput, content: &CriticalFileContent) -> AppResult<Anchor> {
    let start = input.line_start;
    let end = input.line_end.unwrap_or(start);
    if start == 0 || end < start {
        return Err(AppError::invalid("invalid line range"));
    }
    let side = input.side;
    let in_diff_line = |n: u32| content.diff.iter().any(|d| diff_number(d, side) == Some(n));
    let exists = match (side, &content.content) {
        (Side::Right, Some(text)) => (end as usize) <= text.lines().count().max(1),
        // Without the file only diff lines can be commented on. A range may span a hunk gap,
        // so only its ends are checked; posting uses the last line when they are in different
        // hunks.
        _ => in_diff_line(start) && in_diff_line(end),
    };
    if !exists {
        return Err(AppError::invalid(
            "the selected lines are not part of this file",
        ));
    }
    // On the old side only removed lines can be anchored; posting would otherwise move the
    // comment to the new line with the same number.
    let removed = |n: u32| {
        content
            .diff
            .iter()
            .any(|d| d.kind == DiffLineKind::Removed && d.old == Some(n))
    };
    if side == Side::Left && !(removed(start) && removed(end)) {
        return Err(AppError::invalid(
            "comments on the old side must start and end on removed lines",
        ));
    }
    Ok(Anchor {
        start,
        end,
        side,
        in_diff: in_diff_line(end),
    })
}

fn title_of(body: &str) -> String {
    let first = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    if first.chars().count() <= MAX_TITLE_CHARS {
        first.to_string()
    } else {
        let cut: String = first.chars().take(MAX_TITLE_CHARS - 1).collect();
        format!("{}…", cut.trim_end())
    }
}

/// The stored code of a critical file of a finished review, if it can still be commented on.
fn critical_file(svc: &Service, review: &Review, path: &str) -> AppResult<CriticalFileContent> {
    let review_id = review.id.as_str();
    if review.status != ReviewStatus::Done {
        return Err(AppError::invalid("the review has not finished"));
    }
    let meta = svc
        .db
        .list_review_files(review_id)?
        .into_iter()
        .find(|f| f.path == path)
        .ok_or_else(|| {
            AppError::invalid("comments can only be added to the files listed for manual review")
        })?;
    if meta.note == Some(FileNote::Pruned) {
        return Err(AppError::invalid(
            "the code is only kept for the latest review of this PR; comment there",
        ));
    }
    svc.db.get_review_file(review_id, path)
}

fn checked_body(body: &str) -> AppResult<&str> {
    let body = body.trim();
    if body.is_empty() {
        return Err(AppError::invalid("the comment is empty"));
    }
    if body.chars().count() > MAX_BODY_CHARS {
        return Err(AppError::invalid("the comment is too long"));
    }
    Ok(body)
}

/// Stores a comment the user wrote on lines of a critical file of a finished review.
///
/// # Errors
/// Fails when the body is empty or too long, the file is not a critical file of the review (or
/// its code was pruned), or the lines are not part of the stored file or diff.
pub fn add(svc: &Service, input: &UserCommentInput) -> AppResult<Finding> {
    let body = checked_body(&input.body)?;
    let review = svc.db.get_review(&input.review_id)?;
    let content = critical_file(svc, &review, &input.file)?;
    let anchor = validate_anchor(input, &content)?;
    let finding = Finding {
        id: new_id(),
        review_id: input.review_id.clone(),
        position: 0,
        severity: input.severity,
        category: USER_CATEGORY.into(),
        title: title_of(body),
        file: Some(input.file.clone()),
        line_start: Some(anchor.start),
        line_end: (anchor.end != anchor.start).then_some(anchor.end),
        side: anchor.side,
        explanation: String::new(),
        proposed_comment: body.to_string(),
        suggestion: None,
        in_diff: anchor.in_diff,
        diff_excerpt: None,
        selected: true,
        posted_at: None,
        posted_ref: None,
        source: FindingSource::User,
    };
    // No review update event: the review page adds it to its own state, so a refetch can't
    // race with edits that are still being saved.
    svc.db.insert_user_finding(&finding)
}

/// Saves the selection and text of any finding. The title of a user comment is its first line,
/// so it follows the text.
///
/// # Errors
/// Fails when the finding does not exist, a user comment's text is empty or too long, or the
/// database write fails.
pub fn update_finding(svc: &Service, id: &str, selected: bool, text: &str) -> AppResult<Finding> {
    // A user comment keeps the rules it was added with: not empty, not too long.
    if svc.db.get_finding(id)?.source == FindingSource::User {
        checked_body(text)?;
    }
    let finding = svc.db.update_finding(id, selected, text)?;
    let title = title_of(text);
    if finding.source != FindingSource::User || title.is_empty() || title == finding.title {
        return Ok(finding);
    }
    svc.db.set_finding_title(id, &title)
}

/// Deletes one of the user's own comments.
///
/// # Errors
/// Fails for reviewer findings and for comments that were posted already.
pub fn delete(svc: &Service, id: &str) -> AppResult<()> {
    svc.db.delete_user_finding(id)
}

/// A code fence that the text cannot close.
fn fenced(text: &str, lang: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{lang}\n{text}\n{fence}\n")
}

fn render_diff(diff: &[FileDiffLine]) -> String {
    let num = |n: Option<u32>| n.map_or(String::new(), |n| n.to_string());
    let mut out = String::from("old   new   line\n");
    for d in diff {
        let mark = match d.kind {
            DiffLineKind::Added => '+',
            DiffLineKind::Removed => '-',
            DiffLineKind::Context => ' ',
        };
        // Writing to a String cannot fail.
        let _ = writeln!(out, "{:>5} {:>5} {mark}{}", num(d.old), num(d.new), d.text);
    }
    out
}

fn anchor_label(input: &UserCommentInput) -> String {
    let side = match input.side {
        Side::Right => "new file",
        Side::Left => "old file (removed lines)",
    };
    match input.line_end.filter(|e| *e != input.line_start) {
        Some(end) => format!("lines {}-{end} of the {side}", input.line_start),
        None => format!("line {} of the {side}", input.line_start),
    }
}

/// Writes the material for a comment check. The PR path never becomes a filesystem path.
async fn write_material(
    dir: &Path,
    input: &UserCommentInput,
    content: &CriticalFileContent,
) -> AppResult<()> {
    tokio::fs::create_dir_all(dir).await?;
    if let Some(text) = &content.content {
        tokio::fs::write(dir.join("file.txt"), text).await?;
    }
    let mut diff = render_diff(&content.diff);
    if content.diff_truncated {
        diff.push_str("(the diff is cut off here)\n");
    }
    tokio::fs::write(dir.join("file.diff"), diff).await?;
    let comment = format!(
        "File: {}\nAnchored to: {} ({})\n\nDraft comment:\n\n{}",
        input.file,
        anchor_label(input),
        input.severity.as_str(),
        fenced(input.body.trim(), "markdown"),
    );
    tokio::fs::write(dir.join("comment.md"), comment).await?;
    Ok(())
}

fn check_request(
    claude: PathBuf,
    dir: &Path,
    has_file: bool,
    settings: &EffectiveSettings,
) -> RunRequest {
    RunRequest {
        policy: ToolPolicy::Reviewer,
        claude,
        path_env: env::login_path().to_string(),
        cwd: dir.to_path_buf(),
        add_dirs: Vec::new(),
        plugin_dir: None,
        system_prompt: comment_check_system_prompt(settings),
        prompt: comment_check_prompt(has_file),
        schema: comment_check_schema(),
        session_id: uuid::Uuid::new_v4().to_string(),
        model: settings.model.value.clone(),
        effort: settings.effort.value.clone(),
        max_budget_usd: settings.max_budget_usd.value,
        timeout: CHECK_TIMEOUT,
    }
}

/// The model's answer with the comment rules applied in code, never trusted from the prompt.
fn advice_from(
    raw: RawCommentAdvice,
    attribution: Attribution,
    cost_usd: Option<f64>,
) -> CommentAdvice {
    CommentAdvice {
        assessment: raw.assessment,
        feedback: sanitize_dashes(raw.feedback.trim()),
        suggested_comment: sanitize_comment(&raw.suggested_comment, attribution),
        cost_usd,
    }
}

/// Asks a fresh, locked-down Claude run to check the user's comment against the stored code.
///
/// # Errors
/// Fails on the same input checks as [`add`], when the `claude` CLI can't be found, when the run
/// fails, times out or is cancelled, or when its answer does not match the schema.
pub async fn check(
    svc: &Service,
    input: &UserCommentInput,
    cancel: CancellationToken,
) -> AppResult<CommentAdvice> {
    checked_body(&input.body)?;
    let review = svc.db.get_review(&input.review_id)?;
    let content = critical_file(svc, &review, &input.file)?;
    validate_anchor(input, &content)?;
    let settings = svc.effective_settings(&review.server_id, Some(&review.repo))?;
    let config = svc.db.app_config()?;
    let claude = env::resolve_tool("claude", config.claude_path.as_deref())?;

    // A sibling of the review dirs (like the ticket source test), removed as a whole: the
    // review's own dir may already be gone, and recreating it would leave it behind.
    let dir = svc.cache.review_dir(&format!("comment-check-{}", new_id()));
    let result = async {
        write_material(&dir, input, &content).await?;
        let request = check_request(claude, &dir, content.content.is_some(), &settings);
        let outcome = runner::run(request, cancel, |_| {}).await?;
        let raw: RawCommentAdvice = serde_json::from_value(outcome.output)
            .map_err(|e| AppError::Reviewer(format!("unexpected answer format: {e}")))?;
        Ok(advice_from(
            raw,
            Attribution::from_settings(&settings),
            outcome.cost_usd,
        ))
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{SettingsOverrides, resolve};

    fn line(kind: DiffLineKind, old: Option<u32>, new: Option<u32>) -> FileDiffLine {
        FileDiffLine {
            kind,
            old,
            new,
            text: "x".into(),
        }
    }

    fn content(with_file: bool) -> CriticalFileContent {
        CriticalFileContent {
            content: with_file.then(|| "a\nb\nc\nd\ne\n".to_string()),
            diff: vec![
                line(DiffLineKind::Context, Some(1), Some(1)),
                line(DiffLineKind::Removed, Some(2), None),
                line(DiffLineKind::Added, None, Some(2)),
                line(DiffLineKind::Added, None, Some(3)),
                line(DiffLineKind::Context, Some(3), Some(4)),
            ],
            diff_truncated: false,
        }
    }

    fn input(start: u32, end: Option<u32>, side: Side) -> UserCommentInput {
        UserCommentInput {
            review_id: "r".into(),
            file: "a.rs".into(),
            line_start: start,
            line_end: end,
            side,
            severity: Severity::Medium,
            body: "Check this".into(),
        }
    }

    mod validate_anchor {
        use super::*;

        fn check(start: u32, end: Option<u32>, side: Side, with_file: bool) -> AppResult<Anchor> {
            super::validate_anchor(&input(start, end, side), &content(with_file))
        }

        #[test]
        fn accepts_a_range_of_changed_lines_inline() {
            let a = check(2, Some(3), Side::Right, true).unwrap();
            assert_eq!((a.start, a.end, a.in_diff), (2, 3, true));
        }

        #[test]
        fn accepts_file_lines_outside_the_diff_as_general_comments() {
            let a = check(5, None, Side::Right, true).unwrap();
            assert!(!a.in_diff, "line 5 is not in the diff: {a:?}");
        }

        #[test]
        fn rejects_lines_past_the_end_of_the_file() {
            assert!(check(6, None, Side::Right, true).is_err());
        }

        #[test]
        fn rejects_reversed_ranges() {
            assert!(check(3, Some(2), Side::Right, true).is_err());
        }

        #[test]
        fn rejects_line_zero() {
            assert!(check(0, None, Side::Right, true).is_err());
        }

        #[test]
        fn accepts_removed_lines_on_the_old_side() {
            let a = check(2, None, Side::Left, true).unwrap();
            assert!(a.in_diff, "removed line 2 is in the diff: {a:?}");
        }

        #[test]
        fn accepts_removed_ranges_across_a_hunk_gap() {
            let mut c = content(true);
            c.diff.push(line(DiffLineKind::Context, Some(30), Some(31)));
            c.diff.push(line(DiffLineKind::Removed, Some(40), None));
            let a = super::validate_anchor(&input(2, Some(40), Side::Left), &c);
            assert!(a.is_ok(), "rejected: {a:?}");
        }

        #[test]
        fn rejects_context_lines_on_the_old_side() {
            assert!(check(1, None, Side::Left, true).is_err());
        }

        #[test]
        fn rejects_old_side_ranges_ending_on_a_context_line() {
            // Posting would move such a comment to the new line with the same number.
            assert!(check(2, Some(3), Side::Left, true).is_err());
        }

        #[test]
        fn accepts_only_diff_lines_without_the_file() {
            assert!(check(5, None, Side::Right, false).is_err());
            assert!(check(4, None, Side::Right, false).is_ok());
        }
    }

    #[test]
    fn titles_use_the_first_line() {
        assert_eq!(title_of("\n  First line \nmore"), "First line");
        let long = "x".repeat(200);
        assert_eq!(title_of(&long).chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn fences_cannot_be_closed_by_the_text() {
        let f = fenced("a ```` b", "markdown");
        assert!(f.starts_with("`````markdown\n"));
        assert!(fenced("plain", "").starts_with("```\n"));
    }

    #[tokio::test]
    async fn material_uses_fixed_names() {
        let tmp = tempfile::tempdir().unwrap();
        let mut i = input(2, None, Side::Right);
        i.file = "../../evil.rs".into();
        write_material(tmp.path(), &i, &content(true))
            .await
            .unwrap();
        let mut names: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["comment.md", "file.diff", "file.txt"]);
        let comment = std::fs::read_to_string(tmp.path().join("comment.md")).unwrap();
        assert!(comment.contains("line 2 of the new file"));
        let diff = std::fs::read_to_string(tmp.path().join("file.diff")).unwrap();
        assert!(diff.contains("    2       -x"));
    }

    #[test]
    fn check_runs_locked_down() {
        let settings = resolve(
            &SettingsOverrides::default(),
            &SettingsOverrides::default(),
            None,
        );
        let req = check_request("claude".into(), Path::new("/tmp/x"), true, &settings);
        let args = req.args();
        let joined = args.join(" ");
        assert!(joined.contains("--setting-sources user"));
        assert!(args.contains(&"--strict-mcp-config".to_string()));
        assert!(joined.contains("--tools Read Grep Glob Skill"));
        assert!(!joined.contains("Bash"));
        assert!(!args.contains(&"--add-dir".to_string()));
        assert!(req.plugin_dir.is_none());
        assert_eq!(req.cwd, Path::new("/tmp/x"));
    }

    mod advice_from {
        use super::*;

        fn raw(feedback: &str, suggested: &str) -> RawCommentAdvice {
            RawCommentAdvice {
                assessment: CommentAssessment::PartlyValid,
                feedback: feedback.into(),
                suggested_comment: suggested.into(),
            }
        }

        #[test]
        fn keeps_the_assessment_and_cost() {
            let a = super::advice_from(raw("ok", "ok"), Attribution::Forbidden, Some(0.01));
            assert_eq!(
                (a.assessment, a.cost_usd),
                (CommentAssessment::PartlyValid, Some(0.01))
            );
        }

        #[test]
        fn removes_dashes_from_the_feedback() {
            let a = super::advice_from(
                raw("Mostly right — but see line 3", "x"),
                Attribution::Forbidden,
                None,
            );
            assert!(!a.feedback.contains('—'), "feedback: {}", a.feedback);
        }

        #[test]
        fn applies_the_comment_rules_to_the_suggestion() {
            let suggested = "Handle the error — it panics.\n\nCo-Authored-By: Claude <x@y>";
            let a = super::advice_from(raw("x", suggested), Attribution::Forbidden, None);
            assert_eq!(a.suggested_comment, "Handle the error, it panics.");
        }
    }

    #[test]
    fn unknown_assessments_are_rejected_when_parsing() {
        let answer = serde_json::json!({
            "assessment": "great",
            "feedback": "",
            "suggested_comment": "",
        });
        assert!(serde_json::from_value::<RawCommentAdvice>(answer).is_err());
    }
}
