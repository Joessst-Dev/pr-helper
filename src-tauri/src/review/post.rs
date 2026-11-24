//! Posting the findings the user selected.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::db::models::{Finding, FindingSource, ProviderKind, Review, Side};
use crate::error::{AppError, AppResult};
use crate::git::diff::DiffIndex;
use crate::git::repo_cache::FetchSpec;
use crate::providers::{InlineComment, ReviewEvent, ReviewSubmission, fold_comment};
use crate::review::postprocess::finalize_body;
use crate::service::Service;
use crate::settings::EffectiveSettings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SubmitMode {
    Comment,
    RequestChanges,
    Approve,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PostRequest {
    pub review_id: String,
    pub finding_ids: Vec<String>,
    pub mode: SubmitMode,
    /// Review body; `None` posts no summary.
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PostOutcome {
    pub posted: usize,
    pub folded: usize,
    pub review_url: Option<String>,
    pub head_moved: bool,
}

/// (finding id, markdown) for a finding posted in the review body instead of inline.
pub type GeneralComment = (String, String);

/// Build provider comments for the chosen findings. Anything that can't be anchored inline is
/// returned as markdown for the review body.
pub fn build_inline(
    findings: &[Finding],
    index: &DiffIndex,
    settings: &EffectiveSettings,
) -> AppResult<(Vec<InlineComment>, Vec<GeneralComment>)> {
    let mut inline = Vec::new();
    let mut general = Vec::new();
    for f in findings {
        let body = finalize_body(&f.proposed_comment, settings)
            .map_err(|e| AppError::invalid(format!("finding \"{}\": {e}", f.title)))?;
        if body.is_empty() {
            return Err(AppError::invalid(format!(
                "finding \"{}\" has no comment left after applying the comment rules; edit it or deselect it",
                f.title
            )));
        }
        let anchor_line = f.line_end.or(f.line_start);
        let anchor = match (&f.file, anchor_line) {
            (Some(path), Some(line)) => index
                .locate(path, line, f.side)
                // `locate` falls back to the other side for the reviewer's slips. The user
                // picked the side on purpose: a new line outside the diff must not land on a
                // removed line with the same number, it goes into the review body.
                .filter(|a| f.source != FindingSource::User || a.side == f.side),
            _ => None,
        };
        match anchor {
            Some(a) => {
                let line = if a.side == Side::Left {
                    a.old_line
                } else {
                    a.new_line
                }
                .unwrap_or_default();
                // Multi-line ranges need the start on the same side and in the same hunk;
                // otherwise the comment goes on its last line only.
                let start_line = match (f.line_start, f.line_end) {
                    (Some(start), Some(_)) if start < line => index
                        .locate(&a.new_path, start, a.side)
                        .filter(|s| {
                            s.side == a.side && index.same_hunk(&a.new_path, start, line, a.side)
                        })
                        .map(|_| start),
                    _ => None,
                };
                inline.push(InlineComment {
                    finding_id: f.id.clone(),
                    path: a.new_path,
                    old_path: a.old_path,
                    line,
                    start_line,
                    side: a.side,
                    new_line: a.new_line,
                    old_line: a.old_line,
                    body,
                    suggestion: f.suggestion.clone(),
                });
            }
            None => {
                let heading = match (&f.file, f.line_start, f.line_end) {
                    (Some(path), Some(start), Some(end)) if end != start => {
                        format!("**{path}:{start}-{end}**\n\n")
                    }
                    (Some(path), Some(line), _) => format!("**{path}:{line}**\n\n"),
                    (Some(path), None, _) => format!("**{path}**\n\n"),
                    _ => String::new(),
                };
                general.push((f.id.clone(), format!("{heading}{body}")));
            }
        }
    }
    Ok((inline, general))
}

pub async fn post_findings(svc: &Arc<Service>, req: PostRequest) -> AppResult<PostOutcome> {
    let review: Review = svc.db.get_review(&req.review_id)?;
    let (server, provider) = svc.provider_for(&review.server_id)?;
    let settings = svc.effective_settings(&review.server_id, Some(&review.repo))?;

    let findings: Vec<Finding> = svc
        .db
        .list_findings(&review.id)?
        .into_iter()
        .filter(|f| req.finding_ids.contains(&f.id) && f.posted_at.is_none())
        .collect();

    let mut pr = provider.get_pr(&review.project_id, review.number).await?;
    let head_moved = pr.head_sha != review.head_sha;
    if head_moved && req.mode == SubmitMode::Approve && server.kind == ProviderKind::Gitlab {
        // GitLab approves the merge request as a whole, which now includes unreviewed commits.
        return Err(AppError::invalid(
            "new commits arrived after this review; run a new review before approving",
        ));
    }

    // Rebuild the diff index for the reviewed commit to validate anchors.
    let base = review
        .base_sha
        .as_deref()
        .or(pr.base_sha.as_deref())
        .ok_or_else(|| AppError::invalid("unknown base commit for this review"))?;
    let clone_url = pr
        .clone_url
        .as_deref()
        .ok_or_else(|| AppError::invalid("the provider returned no clone URL"))?;
    let auth = provider.git_auth_header();
    let pr_ref = provider.pr_fetch_ref(pr.number);
    let bare = svc
        .cache
        .ensure_pr(&FetchSpec {
            server_id: &review.server_id,
            repo: &review.repo,
            clone_url,
            pr_ref: &pr_ref,
            number: review.number,
            base_ref: &pr.base_ref,
            head_sha: &review.head_sha,
            base_sha: Some(base),
            auth_header: Some(&auth),
        })
        .await?;
    let index = DiffIndex::parse(&svc.cache.diff(&bare, base, &review.head_sha).await?);
    let (mut inline, general) = build_inline(&findings, &index, &settings)?;

    let mut folded_ids: Vec<String> = Vec::with_capacity(general.len());
    let mut body_parts: Vec<String> = Vec::new();
    if let Some(summary) = req.summary.as_deref() {
        let s = finalize_body(summary, &settings)?;
        if !s.is_empty() {
            body_parts.push(s);
        }
    }
    for (id, text) in general {
        folded_ids.push(id);
        body_parts.push(text);
    }

    if head_moved {
        match server.kind {
            // GitHub accepts comments against the reviewed commit.
            ProviderKind::Github => pr.head_sha = review.head_sha.clone(),
            // GitLab positions must match the current diff version; post as general notes.
            ProviderKind::Gitlab => {
                for c in inline.drain(..) {
                    body_parts.push(fold_comment(&c, ""));
                    folded_ids.push(c.finding_id);
                }
            }
        }
    }

    let event = match req.mode {
        SubmitMode::Comment => ReviewEvent::Comment,
        SubmitMode::RequestChanges => ReviewEvent::RequestChanges,
        SubmitMode::Approve => ReviewEvent::Approve,
    };
    if inline.is_empty() && body_parts.is_empty() && event == ReviewEvent::Comment {
        return Err(AppError::invalid(
            "nothing to post: select at least one finding or add a summary",
        ));
    }
    let submission = ReviewSubmission {
        event,
        // The joined body goes through the rules too (fold headings are generated text).
        body: finalize_body(&body_parts.join("\n\n"), &settings)?,
        inline,
    };

    let result = provider.post_review(&pr, &submission).await?;
    // Record whatever reached the server before reporting any failure, so a retry never
    // posts the same comment twice.
    for (finding_id, posted_ref) in &result.posted {
        svc.db.mark_finding_posted(
            finding_id,
            posted_ref.as_deref().or(result.review_url.as_deref()),
        )?;
    }
    if let Some(error) = &result.error {
        svc.publish_review(&review.id);
        return Err(AppError::invalid(format!(
            "posting stopped after {} of {} comment(s): {error}. Posted comments are marked; posting again sends only the rest.",
            result.posted.len(),
            findings.len()
        )));
    }
    for id in &folded_ids {
        svc.db
            .mark_finding_posted(id, result.review_url.as_deref())?;
    }
    if event == ReviewEvent::Approve {
        svc.db.set_approval_posted(&review.id)?;
    }
    svc.publish_review(&review.id);

    Ok(PostOutcome {
        posted: result.posted.len() + folded_ids.len(),
        folded: folded_ids.len() + result.folded.len(),
        review_url: result.review_url,
        head_moved,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::Severity;
    use crate::settings::{SettingsOverrides, resolve};

    const DIFF: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,5 @@\n x\n-old\n+n1\n+n2\n+n3\n z\n";

    fn finding(id: &str, line_start: Option<u32>, line_end: Option<u32>, comment: &str) -> Finding {
        Finding {
            id: id.into(),
            review_id: "r".into(),
            position: 0,
            severity: Severity::High,
            category: "bug".into(),
            title: id.into(),
            file: Some("a.rs".into()),
            line_start,
            line_end,
            side: Side::Right,
            explanation: String::new(),
            proposed_comment: comment.into(),
            suggestion: None,
            in_diff: true,
            diff_excerpt: None,
            selected: true,
            posted_at: None,
            posted_ref: None,
            source: Default::default(),
        }
    }

    #[test]
    fn builds_inline_ranges_and_general_fallbacks() {
        let index = DiffIndex::parse(DIFF);
        let settings = resolve(
            &SettingsOverrides::default(),
            &SettingsOverrides::default(),
            None,
        );
        let findings = vec![
            finding("range", Some(2), Some(4), "Range — issue"),
            finding("single", Some(5), None, "Context line"),
            finding("outside", Some(40), None, "Not in diff"),
        ];
        let (inline, general) = build_inline(&findings, &index, &settings).unwrap();
        assert_eq!(inline.len(), 2);
        assert_eq!((inline[0].line, inline[0].start_line), (4, Some(2)));
        assert_eq!(inline[0].body, "Range, issue");
        assert_eq!(inline[1].old_line, Some(3)); // context line carries both numbers
        assert_eq!(
            general,
            vec![(
                "outside".to_string(),
                "**a.rs:40**\n\nNot in diff".to_string()
            )]
        );
    }

    #[test]
    fn user_comments_outside_the_diff_never_move_to_the_other_side() {
        let index = DiffIndex::parse(DIFF);
        let settings = resolve(
            &SettingsOverrides::default(),
            &SettingsOverrides::default(),
            None,
        );
        // New line 9 is not in the diff; old line 2 is removed. A reviewer finding on right
        // line 2 would be accepted on the left, a user comment on right line 9 has no match.
        let mut mine = finding("mine", Some(9), None, "Unchanged line");
        mine.source = FindingSource::User;
        let (inline, general) = build_inline(&[mine], &index, &settings).unwrap();
        assert!(inline.is_empty(), "posted inline: {inline:?}");
        assert_eq!(general[0].1, "**a.rs:9**\n\nUnchanged line");
    }

    #[test]
    fn user_comments_do_not_fall_back_to_a_removed_line_with_the_same_number() {
        // Old line 3 is removed; new line 3 is outside this diff.
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -3,1 +2,0 @@\n-gone\n";
        let index = DiffIndex::parse(diff);
        let settings = resolve(
            &SettingsOverrides::default(),
            &SettingsOverrides::default(),
            None,
        );
        let mut mine = finding("mine", Some(3), None, "About new line 3");
        mine.source = FindingSource::User;
        let (inline, general) = build_inline(&[mine], &index, &settings).unwrap();
        assert!(inline.is_empty(), "posted inline: {inline:?}");
        assert_eq!(general.len(), 1);
    }

    #[test]
    fn general_comments_on_a_range_name_the_whole_range() {
        let index = DiffIndex::parse(DIFF);
        let settings = resolve(
            &SettingsOverrides::default(),
            &SettingsOverrides::default(),
            None,
        );
        let (_, general) = build_inline(
            &[finding("f", Some(20), Some(24), "Block")],
            &index,
            &settings,
        )
        .unwrap();
        assert_eq!(general[0].1, "**a.rs:20-24**\n\nBlock");
    }

    #[test]
    fn ranges_across_hunks_post_on_their_last_line() {
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n\
@@ -1,2 +1,2 @@\n x\n-old\n+new\n@@ -40,2 +40,2 @@\n y\n-old2\n+new2\n";
        let index = DiffIndex::parse(diff);
        let settings = resolve(
            &SettingsOverrides::default(),
            &SettingsOverrides::default(),
            None,
        );
        let (inline, _) = build_inline(
            &[finding("f", Some(2), Some(41), "Both")],
            &index,
            &settings,
        )
        .unwrap();
        assert_eq!((inline[0].line, inline[0].start_line), (41, None));
    }

    #[test]
    fn user_comments_on_removed_lines_anchor_on_the_left() {
        let index = DiffIndex::parse(DIFF);
        let settings = resolve(
            &SettingsOverrides::default(),
            &SettingsOverrides::default(),
            None,
        );
        let mut removed = finding("mine", Some(2), None, "Why was this removed?");
        removed.side = Side::Left;
        removed.source = crate::db::models::FindingSource::User;
        let (inline, general) = build_inline(&[removed], &index, &settings).unwrap();
        assert!(general.is_empty());
        assert_eq!((inline[0].side, inline[0].line), (Side::Left, 2));
        assert_eq!(inline[0].old_line, Some(2));
    }

    #[test]
    fn edited_comments_are_checked_again() {
        let index = DiffIndex::parse(DIFF);
        let settings = resolve(
            &SettingsOverrides::default(),
            &SettingsOverrides::default(),
            None,
        );
        let findings = vec![finding(
            "f",
            Some(2),
            None,
            "Fix this.\n\nCo-Authored-By: Claude <x@y>",
        )];
        let (inline, _) = build_inline(&findings, &index, &settings).unwrap();
        assert_eq!(inline[0].body, "Fix this.");
    }
}
