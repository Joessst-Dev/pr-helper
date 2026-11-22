//! Comments the user writes on the critical files, and the optional AI check of such a comment.
//!
//! A user comment is stored as a finding with `source = user`, so selecting, editing and posting
//! (including `finalize_body`) work exactly as for the reviewer's findings.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::db::models::{
    CriticalFileContent, DiffLineKind, FileDiffLine, FileNote, Finding, FindingSource,
    ReviewStatus, Severity, Side,
};
use crate::db::new_id;
use crate::env;
use crate::error::{AppError, AppResult};
use crate::review::postprocess::{sanitize_comment, sanitize_dashes};
use crate::review::prompt::{comment_check_prompt, comment_check_system_prompt};
use crate::review::runner::{self, RunRequest, ToolPolicy};
use crate::review::schema::{RawCommentAdvice, comment_check_schema};
use crate::service::Service;

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
        // Without the file only diff lines can be commented on.
        _ => (start..=end).all(in_diff_line),
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
fn critical_file(svc: &Service, review_id: &str, path: &str) -> AppResult<CriticalFileContent> {
    let review = svc.db.get_review(review_id)?;
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

pub fn add(svc: &Service, input: &UserCommentInput) -> AppResult<Finding> {
    let body = checked_body(&input.body)?;
    let content = critical_file(svc, &input.review_id, &input.file)?;
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
        out.push_str(&format!(
            "{:>5} {:>5} {mark}{}\n",
            num(d.old),
            num(d.new),
            d.text
        ));
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
    settings: &crate::settings::EffectiveSettings,
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

fn advice_from(
    raw: RawCommentAdvice,
    forbid_attribution: bool,
    cost_usd: Option<f64>,
) -> AppResult<CommentAdvice> {
    let assessment = match raw.assessment.as_str() {
        "valid" => CommentAssessment::Valid,
        "partly_valid" => CommentAssessment::PartlyValid,
        "not_valid" => CommentAssessment::NotValid,
        other => {
            return Err(AppError::Reviewer(format!(
                "unexpected assessment \"{other}\""
            )));
        }
    };
    Ok(CommentAdvice {
        assessment,
        feedback: sanitize_dashes(raw.feedback.trim()),
        suggested_comment: sanitize_comment(&raw.suggested_comment, forbid_attribution),
        cost_usd,
    })
}

/// Asks a fresh, locked-down Claude run to check the user's comment against the stored code.
pub async fn check(
    svc: &Arc<Service>,
    input: &UserCommentInput,
    cancel: CancellationToken,
) -> AppResult<CommentAdvice> {
    checked_body(&input.body)?;
    let content = critical_file(svc, &input.review_id, &input.file)?;
    validate_anchor(input, &content)?;
    let review = svc.db.get_review(&input.review_id)?;
    let settings = svc.effective_settings(&review.server_id, Some(&review.repo))?;
    let config = svc.db.app_config()?;
    let claude = env::resolve_tool("claude", config.claude_path.as_deref())?;

    let dir = svc
        .cache
        .review_dir(&input.review_id)
        .join("comment-checks")
        .join(new_id());
    let result = async {
        write_material(&dir, input, &content).await?;
        let request = check_request(claude, &dir, content.content.is_some(), &settings);
        let outcome = runner::run(request, cancel, |_| {}).await?;
        let raw: RawCommentAdvice = serde_json::from_value(outcome.output)
            .map_err(|e| AppError::Reviewer(format!("unexpected answer format: {e}")))?;
        advice_from(raw, settings.forbid_ai_attribution.value, outcome.cost_usd)
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

    #[test]
    fn anchors_are_validated_against_file_and_diff() {
        let c = content(true);
        let a = validate_anchor(&input(2, Some(3), Side::Right), &c).unwrap();
        assert_eq!((a.start, a.end, a.in_diff), (2, 3, true));
        // In the file but outside the diff: allowed, posted as a general comment.
        let a = validate_anchor(&input(5, None, Side::Right), &c).unwrap();
        assert!(!a.in_diff);
        assert!(validate_anchor(&input(6, None, Side::Right), &c).is_err());
        assert!(validate_anchor(&input(3, Some(2), Side::Right), &c).is_err());
        assert!(validate_anchor(&input(0, None, Side::Right), &c).is_err());
        // Removed lines use the old side.
        let a = validate_anchor(&input(2, None, Side::Left), &c).unwrap();
        assert!(a.in_diff);
        assert!(validate_anchor(&input(1, None, Side::Left), &c).is_err());
        // A range ending on a context line would be posted on the new side.
        assert!(validate_anchor(&input(2, Some(3), Side::Left), &c).is_err());
        // Without the file, only diff lines.
        let c = content(false);
        assert!(validate_anchor(&input(5, None, Side::Right), &c).is_err());
        assert!(validate_anchor(&input(4, None, Side::Right), &c).is_ok());
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

    #[test]
    fn advice_is_sanitized() {
        let raw = RawCommentAdvice {
            assessment: "partly_valid".into(),
            feedback: "Mostly right — but see line 3".into(),
            suggested_comment: "Handle the error — it panics.\n\nCo-Authored-By: Claude <x@y>"
                .into(),
        };
        let a = advice_from(raw, true, Some(0.01)).unwrap();
        assert_eq!(a.assessment, CommentAssessment::PartlyValid);
        assert!(!a.feedback.contains('—'));
        assert_eq!(a.suggested_comment, "Handle the error, it panics.");
        let bad = RawCommentAdvice {
            assessment: "great".into(),
            feedback: String::new(),
            suggested_comment: String::new(),
        };
        assert!(advice_from(bad, true, None).is_err());
    }
}
