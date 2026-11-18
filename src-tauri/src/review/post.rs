//! Posting the findings the user selected.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::db::models::{Finding, ProviderKind, Review, Side};
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
            continue;
        }
        let anchor_line = f.line_end.or(f.line_start);
        let anchor = match (&f.file, anchor_line) {
            (Some(path), Some(line)) => index.locate(path, line, f.side),
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
                // Multi-line ranges need the start on the same side and inside the diff.
                let start_line = match (f.line_start, f.line_end) {
                    (Some(start), Some(_)) if start < line => index
                        .locate(&a.new_path, start, a.side)
                        .filter(|s| s.side == a.side)
                        .map(|_| start),
                    _ => None,
                };
                inline.push(InlineComment {
                    finding_id: f.id.clone(),
                    path: a.new_path.clone(),
                    old_path: a.old_path.clone(),
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
                let heading = match (&f.file, f.line_start) {
                    (Some(path), Some(line)) => format!("**{path}:{line}**\n\n"),
                    (Some(path), None) => format!("**{path}**\n\n"),
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

    // Rebuild the diff index for the reviewed commit to validate anchors.
    let base = review
        .base_sha
        .clone()
        .or_else(|| pr.base_sha.clone())
        .ok_or_else(|| AppError::invalid("unknown base commit for this review"))?;
    let clone_url = pr
        .clone_url
        .clone()
        .ok_or_else(|| AppError::invalid("the provider returned no clone URL"))?;
    let auth = provider.git_auth_header();
    let pr_ref = provider.pr_fetch_ref(pr.number);
    let bare = svc
        .cache
        .ensure_pr(&FetchSpec {
            server_id: &review.server_id,
            repo: &review.repo,
            clone_url: &clone_url,
            pr_ref: &pr_ref,
            number: review.number,
            base_ref: &pr.base_ref,
            head_sha: &review.head_sha,
            base_sha: Some(&base),
            auth_header: Some(&auth),
        })
        .await?;
    let index = DiffIndex::parse(&svc.cache.diff(&bare, &base, &review.head_sha).await?);
    let (mut inline, general) = build_inline(&findings, &index, &settings)?;

    let mut folded_ids: Vec<String> = general.iter().map(|(id, _)| id.clone()).collect();
    let mut body_parts: Vec<String> = Vec::new();
    if let Some(summary) = req.summary.as_deref() {
        let s = finalize_body(summary, &settings)?;
        if !s.is_empty() {
            body_parts.push(s);
        }
    }
    body_parts.extend(general.into_iter().map(|(_, text)| text));

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
        body: body_parts.join("\n\n"),
        inline,
    };
    // The joined body must also pass the rules (fold headings are generated text).
    finalize_body(&submission.body, &settings)?;

    let result = provider.post_review(&pr, &submission).await?;
    for (finding_id, posted_ref) in &result.posted {
        svc.db.mark_finding_posted(
            finding_id,
            posted_ref.as_deref().or(result.review_url.as_deref()),
        )?;
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
        posted: findings.len(),
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
