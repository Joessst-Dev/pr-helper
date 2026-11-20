//! One review run: fetch, prepare context, run Claude, store findings, follow-up actions.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::db::models::{PrState, PullRequest, ReviewStatus, ReviewTrigger, TicketRef, Verdict};
use crate::env;
use crate::error::{AppError, AppResult};
use crate::git::diff::DiffIndex;
use crate::git::repo_cache::FetchSpec;
use crate::review::guard::steering_attempt;
use crate::review::postprocess::{finalize_body, process};
use crate::review::prompt::{PreviousReview, PromptInput, system_prompt, user_prompt};
use crate::review::runner::{self, RunEvent, RunRequest, ToolPolicy};
use crate::review::schema::{RawReview, review_schema};
use crate::review::skills::build_plugin;
use crate::service::{ProgressKind, Service, TicketPlan};
use crate::settings::EffectiveSettings;
use crate::tickets::{Ticket, render};

pub async fn run_review(
    svc: &Arc<Service>,
    review_id: &str,
    pr: PullRequest,
    trigger: ReviewTrigger,
    cancel: CancellationToken,
) {
    let _ = svc
        .db
        .set_review_status(review_id, ReviewStatus::Running, None);
    svc.publish_review(review_id);
    svc.progress(review_id, ProgressKind::Status, "Started");

    let review_dir = svc.cache.review_dir(review_id);
    let outcome = execute(svc, review_id, &pr, &review_dir, cancel.clone()).await;

    let keep = svc
        .db
        .app_config()
        .map(|c| c.keep_worktrees)
        .unwrap_or(false);
    let worktree = review_dir.join("worktree");
    match &outcome {
        Ok(_) if keep => {
            let _ = svc
                .db
                .set_review_worktree(review_id, Some(&worktree.to_string_lossy()));
        }
        _ => cleanup(svc, &pr, &review_dir).await,
    }

    match outcome {
        Ok((verdict, steering)) => {
            svc.progress(
                review_id,
                ProgressKind::Status,
                format!("Finished: {}", verdict.as_str()),
            );
            svc.publish_review(review_id);
            svc.sink.tracked_updated();
            after_review(svc, review_id, &pr, verdict, trigger, steering.as_deref()).await;
        }
        Err(e) if cancel.is_cancelled() => {
            let _ =
                svc.db
                    .set_review_status(review_id, ReviewStatus::Cancelled, Some(&e.to_string()));
            svc.progress(review_id, ProgressKind::Status, "Cancelled");
            svc.publish_review(review_id);
        }
        Err(e) => {
            let msg = e.to_string();
            let _ = svc
                .db
                .set_review_status(review_id, ReviewStatus::Failed, Some(&msg));
            svc.progress(review_id, ProgressKind::Error, &msg);
            svc.publish_review(review_id);
            svc.sink.notify(
                &format!("Review failed: {}#{}", pr.repo, pr.number),
                &msg.chars().take(200).collect::<String>(),
            );
        }
    }
}

async fn cleanup(svc: &Service, pr: &PullRequest, review_dir: &Path) {
    let bare = svc.cache.bare_path(&pr.server_id, &pr.repo);
    let worktree = review_dir.join("worktree");
    if worktree.exists() {
        let _ = svc.cache.remove_worktree(&bare, &worktree).await;
    }
    let _ = tokio::fs::remove_dir_all(review_dir).await;
}

async fn execute(
    svc: &Arc<Service>,
    review_id: &str,
    pr: &PullRequest,
    review_dir: &Path,
    cancel: CancellationToken,
) -> AppResult<(Verdict, Option<String>)> {
    let (_server, provider) = svc.provider_for(&pr.server_id)?;
    let settings = svc.effective_settings(&pr.server_id, Some(&pr.repo))?;
    let config = svc.db.app_config()?;
    let claude = env::resolve_tool("claude", config.claude_path.as_deref())?;
    if let Ok(git) = env::resolve_tool("git", config.git_path.as_deref()) {
        svc.cache.set_git(git);
    }

    let clone_url = pr.clone_url.clone().ok_or_else(|| {
        AppError::invalid("the provider returned no clone URL for this repository")
    })?;

    svc.progress(review_id, ProgressKind::Status, "Fetching repository");
    let auth = provider.git_auth_header();
    let pr_ref = provider.pr_fetch_ref(pr.number);
    let bare = svc
        .cache
        .ensure_pr(&FetchSpec {
            server_id: &pr.server_id,
            repo: &pr.repo,
            clone_url: &clone_url,
            pr_ref: &pr_ref,
            number: pr.number,
            base_ref: &pr.base_ref,
            head_sha: &pr.head_sha,
            base_sha: pr.base_sha.as_deref(),
            auth_header: Some(&auth),
        })
        .await?;
    check_cancel(&cancel)?;

    let base = svc
        .cache
        .base_commit(&bare, pr.base_sha.as_deref(), &pr.base_ref)
        .await?;
    svc.db.set_review_base(review_id, &base)?;
    let diff = svc.cache.diff(&bare, &base, &pr.head_sha).await?;
    let index = DiffIndex::parse(&diff);
    let merge_base = svc
        .cache
        .merge_base(&bare, &base, &pr.head_sha)
        .await
        .unwrap_or_else(|_| base.clone());
    let commits = svc
        .cache
        .commit_log(&bare, &merge_base, &pr.head_sha)
        .await
        .unwrap_or_default();

    let tickets = load_tickets(svc, review_id, pr, &settings, review_dir, &cancel).await;
    check_cancel(&cancel)?;

    let steering = steering_attempt(
        &pr.title,
        &pr.description,
        &commits,
        &diff,
        &render::guard_text(&tickets),
    );

    // Context files live next to (not inside) the worktree so they never show up as repo files.
    let context_dir = review_dir.join("context");
    tokio::fs::create_dir_all(&context_dir).await?;
    let has_tickets = !tickets.is_empty();
    if has_tickets {
        tokio::fs::write(context_dir.join("tickets.md"), render::render(&tickets)).await?;
        let refs: Vec<TicketRef> = tickets
            .iter()
            .map(|t| TicketRef {
                key: t.key.clone(),
                url: t.url.clone(),
            })
            .collect();
        svc.db.set_review_tickets(review_id, &refs)?;
        svc.publish_review(review_id);
    }
    tokio::fs::write(context_dir.join("pr.diff"), &diff).await?;
    tokio::fs::write(context_dir.join("commits.txt"), &commits).await?;
    tokio::fs::write(
        context_dir.join("description.md"),
        format!("# {}\n\n{}", pr.title, pr.description),
    )
    .await?;

    let previous = svc
        .db
        .last_done_review(&pr.server_id, &pr.repo, pr.number)?;
    let previous_findings = match &previous {
        Some(p) => svc.db.list_findings(&p.id)?,
        None => Vec::new(),
    };
    let mut has_incremental = false;
    if let Some(prev) = previous.as_ref().filter(|p| p.head_sha != pr.head_sha)
        && let Ok(inc) = svc
            .cache
            .diff_between(&bare, &prev.head_sha, &pr.head_sha)
            .await
    {
        tokio::fs::write(context_dir.join("incremental.diff"), inc).await?;
        has_incremental = true;
    }

    svc.progress(review_id, ProgressKind::Status, "Checking out PR head");
    let worktree = review_dir.join("worktree");
    svc.cache
        .add_worktree(&bare, &worktree, &pr.head_sha)
        .await?;
    check_cancel(&cancel)?;

    let skills = svc.db.skills_for_review(&pr.server_id, &pr.repo)?;
    let plugin = build_plugin(&skills, &review_dir.join("plugin")).await?;
    let (plugin_dir, skill_infos): (Option<PathBuf>, Vec<_>) = match plugin {
        Some((dir, infos)) => (Some(dir), infos),
        None => (None, Vec::new()),
    };
    if !skill_infos.is_empty() {
        svc.progress(
            review_id,
            ProgressKind::Status,
            format!("Loaded {} review skill(s)", skill_infos.len()),
        );
    }

    let prompt = user_prompt(&PromptInput {
        pr,
        base: &base,
        head: &pr.head_sha,
        settings: &settings,
        skills: &skill_infos,
        context_dir: &context_dir,
        changed_files: index.paths().count(),
        previous: previous.as_ref().map(|p| PreviousReview {
            reviewed_sha: &p.head_sha,
            verdict: p.verdict,
            findings: &previous_findings,
        }),
        has_incremental_diff: has_incremental,
        has_tickets,
    });

    let session_id = uuid::Uuid::new_v4().to_string();
    svc.db.set_review_session(review_id, &session_id)?;
    svc.progress(review_id, ProgressKind::Status, "Reviewer running");

    let request = RunRequest {
        policy: ToolPolicy::Reviewer,
        claude,
        path_env: env::login_path().to_string(),
        cwd: worktree.clone(),
        add_dirs: vec![context_dir.clone()],
        plugin_dir,
        system_prompt: system_prompt(&settings, has_tickets),
        prompt,
        schema: review_schema(),
        session_id,
        model: settings.model.value.clone(),
        effort: settings.effort.value.clone(),
        max_budget_usd: settings.max_budget_usd.value,
        timeout: Duration::from_secs(u64::from(config.review_timeout_mins.max(1)) * 60),
    };
    let svc_events = svc.clone();
    let rid = review_id.to_string();
    let outcome = runner::run(request, cancel, move |event| match event {
        RunEvent::Started { model, .. } => {
            svc_events.progress(&rid, ProgressKind::Status, format!("Model: {model}"))
        }
        RunEvent::Tool { name, detail } => {
            let detail = detail.replace(&worktree.to_string_lossy().to_string(), ".");
            svc_events.progress(
                &rid,
                ProgressKind::Tool,
                format!("{name} {detail}").trim().to_string(),
            )
        }
        RunEvent::Text(text) => svc_events.progress(&rid, ProgressKind::Text, text),
    })
    .await?;

    let raw: RawReview = serde_json::from_value(outcome.output)
        .map_err(|e| AppError::Reviewer(format!("unexpected review format: {e}")))?;
    let processed = process(raw, review_id, &settings, &index, has_tickets);
    svc.db.complete_review(
        review_id,
        processed.verdict,
        &processed.summary,
        outcome.cost_usd,
        &processed.findings,
    )?;
    svc.db.set_tracked_reviewed(
        &pr.server_id,
        &pr.repo,
        pr.number,
        &pr.head_sha,
        Some(processed.verdict),
    )?;
    Ok((processed.verdict, steering))
}

/// Loads the tickets linked from the PR, if a ticket source is configured. Never fails the
/// review: problems are reported in the review log and the review runs without them.
async fn load_tickets(
    svc: &Arc<Service>,
    review_id: &str,
    pr: &PullRequest,
    settings: &EffectiveSettings,
    review_dir: &Path,
    cancel: &CancellationToken,
) -> Vec<Ticket> {
    let (source, keys) = match svc.ticket_plan(settings, pr) {
        TicketPlan::NotConfigured => return Vec::new(),
        TicketPlan::Skipped {
            message, is_error, ..
        } => {
            let kind = if is_error {
                ProgressKind::Error
            } else {
                ProgressKind::Status
            };
            svc.progress(review_id, kind, message);
            return Vec::new();
        }
        TicketPlan::Ready { source, keys } => (source, keys),
    };
    let mut loaded = Vec::new();
    for key in keys {
        if cancel.is_cancelled() {
            break;
        }
        svc.progress(
            review_id,
            ProgressKind::Status,
            format!("Loading ticket {key}"),
        );
        let work_dir = review_dir.join("ticket-fetch");
        match svc
            .fetch_ticket(&source, &key, &work_dir, cancel.clone())
            .await
        {
            Ok(t) => loaded.push(t),
            Err(e) => svc.progress(
                review_id,
                ProgressKind::Error,
                format!("Ticket {key} could not be loaded: {e}"),
            ),
        }
        let _ = tokio::fs::remove_dir_all(&work_dir).await;
    }
    loaded
}

fn check_cancel(cancel: &CancellationToken) -> AppResult<()> {
    if cancel.is_cancelled() {
        Err(AppError::Reviewer("cancelled".into()))
    } else {
        Ok(())
    }
}

/// Notifications and auto-approval once a review is stored.
async fn after_review(
    svc: &Arc<Service>,
    review_id: &str,
    pr: &PullRequest,
    verdict: Verdict,
    trigger: ReviewTrigger,
    steering: Option<&str>,
) {
    let Ok(settings) = svc.effective_settings(&pr.server_id, Some(&pr.repo)) else {
        return;
    };
    let finding_count = svc
        .db
        .list_findings(review_id)
        .map(|f| f.len())
        .unwrap_or(0);
    let label = format!("{}#{}", pr.repo, pr.number);

    if verdict == Verdict::Approve
        && settings.auto_approve_on_approve_verdict.value
        && let Some(excerpt) = steering
    {
        // The model may have been talked into approving; a human has to decide.
        svc.progress(
            review_id,
            ProgressKind::Error,
            format!("Auto-approve skipped: the PR text addresses the reviewer (\"{excerpt}\")"),
        );
        svc.sink.notify(
            &format!("Approve manually? {label}"),
            "The reviewer approved, but the PR text seems to address the reviewer, so nothing was posted automatically.",
        );
        return;
    }

    if verdict == Verdict::Approve && settings.auto_approve_on_approve_verdict.value {
        match auto_approve(svc, review_id, pr, &settings).await {
            Ok(true) => {
                svc.sink.notify(
                    &format!("Approved {label}"),
                    &format!(
                        "The reviewer approved \"{}\" and the approval was posted.",
                        pr.title
                    ),
                );
            }
            Ok(false) => {
                svc.sink.notify(
                    &format!("Not approved: {label}"),
                    "New commits arrived after the review, so the approval was not posted.",
                );
            }
            Err(e) => {
                svc.progress(
                    review_id,
                    ProgressKind::Error,
                    format!("Auto-approve failed: {e}"),
                );
                svc.sink
                    .notify(&format!("Auto-approve failed: {label}"), &e.to_string());
            }
        }
        return;
    }

    let trigger_label = if trigger == ReviewTrigger::Auto {
        "Auto-review"
    } else {
        "Review"
    };
    let verdict_label = match verdict {
        Verdict::Approve => "approve",
        Verdict::RequestChanges => "changes requested",
        Verdict::Comment => "comments",
    };
    svc.sink.notify(
        &format!("{trigger_label} finished: {label}"),
        &format!(
            "Verdict: {verdict_label}, {finding_count} finding(s) waiting for your selection."
        ),
    );
}

/// Returns Ok(false) when the PR moved on since the review.
async fn auto_approve(
    svc: &Arc<Service>,
    review_id: &str,
    pr: &PullRequest,
    settings: &EffectiveSettings,
) -> AppResult<bool> {
    let (_server, provider) = svc.provider_for(&pr.server_id)?;
    let fresh = provider.get_pr(&pr.project_id, pr.number).await?;
    if fresh.head_sha != pr.head_sha || fresh.state != PrState::Open {
        return Ok(false);
    }
    let review = svc.db.get_review(review_id)?;
    let body = finalize_body(review.summary.as_deref().unwrap_or_default(), settings)?;
    provider.approve(&fresh, &body).await?;
    svc.db.set_approval_posted(review_id)?;
    svc.progress(review_id, ProgressKind::Status, "Approval posted");
    svc.publish_review(review_id);
    Ok(true)
}
