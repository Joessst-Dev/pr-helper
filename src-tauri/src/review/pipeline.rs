//! One review run: fetch, prepare context, run Claude, store findings, follow-up actions.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::db::models::{
    CriticalFile, CriticalFileContent, FileNote, Finding, FindingSource, PrState, PullRequest,
    Review, ReviewFile, ReviewStatus, ReviewTrigger, TicketRef, Verdict,
};
use crate::env;
use crate::error::{AppError, AppResult};
use crate::git::diff::DiffIndex;
use crate::git::repo_cache::{FetchSpec, TreeEntry};
use crate::providers::GitProvider;
use crate::review::guard::steering_attempt;
use crate::review::postprocess::{Processed, finalize_body, process};
use crate::review::prompt::SkillInfo;
use crate::review::prompt::{PreviousReview, PromptInput, system_prompt, user_prompt};
use crate::review::runner::{self, RunEvent, RunOutcome, RunRequest, ToolPolicy};
use crate::review::schema::{RawReview, review_schema};
use crate::review::skills::build_plugin;
use crate::service::{ProgressKind, Service, TicketPlan};
use crate::settings::EffectiveSettings;
use crate::tickets::{Ticket, render};

/// Runs one queued review to the end. It never fails: the outcome is recorded on the review
/// (done, failed or cancelled), and the worktree is removed unless the user keeps worktrees.
pub async fn run_review(
    svc: &Service,
    review_id: &str,
    pr: PullRequest,
    trigger: ReviewTrigger,
    cancel: CancellationToken,
) {
    svc.record_review_status(review_id, ReviewStatus::Running, None);
    svc.publish_review(review_id);
    svc.progress(review_id, ProgressKind::Status, "Started");

    let review_dir = svc.cache.review_dir(review_id);
    let outcome = execute(svc, review_id, &pr, &review_dir, cancel.clone()).await;

    let keep = svc.db.app_config().is_ok_and(|c| c.keep_worktrees);
    let worktree = review_dir.join("worktree");
    match &outcome {
        Ok(_) if keep => {
            if let Err(e) = svc
                .db
                .set_review_worktree(review_id, Some(&worktree.to_string_lossy()))
            {
                tracing::warn!("could not record the kept worktree of review {review_id}: {e}");
            }
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
            svc.record_review_status(review_id, ReviewStatus::Cancelled, Some(&e.to_string()));
            svc.progress(review_id, ProgressKind::Status, "Cancelled");
            svc.publish_review(review_id);
        }
        Err(e) => {
            let msg = e.to_string();
            svc.record_review_status(review_id, ReviewStatus::Failed, Some(&msg));
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
    let bare_repo = svc.cache.bare_path(&pr.server_id, &pr.repo);
    let worktree = review_dir.join("worktree");
    if worktree.exists()
        && let Err(e) = svc.cache.remove_worktree(&bare_repo, &worktree).await
    {
        tracing::debug!("could not remove worktree {}: {e}", worktree.display());
    }
    let _ = tokio::fs::remove_dir_all(review_dir).await;
}

async fn execute(
    svc: &Service,
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

    let fetched = fetch_and_diff(svc, review_id, pr, provider.as_ref(), &cancel).await?;

    let tickets = load_tickets(svc, review_id, pr, &settings, review_dir, &cancel).await;
    check_cancel(&cancel)?;
    let steering = steering_attempt(
        &pr.title,
        &pr.description,
        &fetched.commits,
        &fetched.diff,
        &render::guard_text(&tickets),
    );

    // Context files live next to (not inside) the worktree so they never show up as repo files.
    let context_dir = review_dir.join("context");
    let has_tickets = !tickets.is_empty();
    write_context(svc, review_id, pr, &context_dir, &fetched, tickets).await?;

    let previous = previous_review(svc, pr)?;
    let has_incremental = match previous.as_ref().filter(|(p, _)| p.head_sha != pr.head_sha) {
        Some((prev, _)) => {
            write_incremental_diff(svc, &fetched.bare_repo, &prev.head_sha, pr, &context_dir)
                .await?
        }
        None => false,
    };

    svc.progress(review_id, ProgressKind::Status, "Checking out PR head");
    let worktree = review_dir.join("worktree");
    svc.cache
        .add_worktree(&fetched.bare_repo, &worktree, &pr.head_sha)
        .await?;
    check_cancel(&cancel)?;

    let (plugin_dir, skill_infos) = prepare_skills(svc, review_id, pr, review_dir).await?;
    let prompt = user_prompt(&PromptInput {
        pr,
        base: &fetched.base,
        head: &pr.head_sha,
        settings: &settings,
        skills: &skill_infos,
        context_dir: &context_dir,
        changed_files: fetched.index.len(),
        previous: previous.as_ref().map(|(p, findings)| PreviousReview {
            reviewed_sha: &p.head_sha,
            verdict: p.verdict,
            findings,
        }),
        has_incremental_diff: has_incremental,
        has_tickets,
    });

    let session_id = uuid::Uuid::new_v4().to_string();
    svc.db.set_review_session(review_id, &session_id)?;
    let request = RunRequest {
        policy: ToolPolicy::Reviewer,
        claude,
        path_env: env::login_path().to_string(),
        cwd: worktree,
        add_dirs: vec![context_dir],
        plugin_dir,
        system_prompt: system_prompt(&settings, has_tickets),
        prompt,
        schema: review_schema(),
        session_id,
        model: settings.model.value.clone(),
        effort: settings.effort.value.clone(),
        max_budget_usd: settings.max_budget_usd.value,
        timeout: Duration::from_mins(config.review_timeout_mins.max(1).into()),
    };
    let outcome = run_reviewer(svc, review_id, request, cancel.clone()).await?;

    let raw: RawReview = serde_json::from_value(outcome.output)
        .map_err(|e| AppError::Reviewer(format!("unexpected review format: {e}")))?;
    let mut processed = process(raw, review_id, &settings, &fetched.index, has_tickets);
    let files = snapshot_files(
        svc,
        review_id,
        &fetched.bare_repo,
        &pr.head_sha,
        &fetched.index,
        std::mem::take(&mut processed.critical_files),
        &cancel,
    )
    .await?;
    check_cancel(&cancel)?;
    store_review(svc, review_id, pr, &processed, outcome.cost_usd, &files)?;
    Ok((processed.verdict, steering))
}

/// Stores the finished review and records its head as reviewed for the PR's tracking.
fn store_review(
    svc: &Service,
    review_id: &str,
    pr: &PullRequest,
    processed: &Processed,
    cost_usd: Option<f64>,
    files: &[ReviewFile],
) -> AppResult<()> {
    svc.db.complete_review(
        review_id,
        processed.verdict,
        &processed.summary,
        cost_usd,
        &processed.findings,
        files,
    )?;
    svc.db.set_tracked_reviewed(
        &pr.server_id,
        &pr.repo,
        pr.number,
        &pr.head_sha,
        Some(processed.verdict),
    )
}

/// The last finished review of the PR, with the findings the reviewer reported in it. The
/// user's own comments are not the reviewer's to re-report.
fn previous_review(svc: &Service, pr: &PullRequest) -> AppResult<Option<(Review, Vec<Finding>)>> {
    let Some(previous) = svc
        .db
        .last_done_review(&pr.server_id, &pr.repo, pr.number)?
    else {
        return Ok(None);
    };
    let findings = svc
        .db
        .list_findings(&previous.id)?
        .into_iter()
        .filter(|f| f.source == FindingSource::Reviewer)
        .collect();
    Ok(Some((previous, findings)))
}

/// The PR's commits in the local cache, and what changed between base and head.
struct Fetched {
    bare_repo: PathBuf,
    base: String,
    diff: String,
    index: DiffIndex,
    commits: String,
}

async fn fetch_and_diff(
    svc: &Service,
    review_id: &str,
    pr: &PullRequest,
    provider: &dyn GitProvider,
    cancel: &CancellationToken,
) -> AppResult<Fetched> {
    let clone_url = pr.clone_url.as_deref().ok_or_else(|| {
        AppError::invalid("the provider returned no clone URL for this repository")
    })?;
    svc.progress(review_id, ProgressKind::Status, "Fetching repository");
    let auth = provider.git_auth_header();
    let pr_ref = provider.pr_fetch_ref(pr.number);
    let bare_repo = svc
        .cache
        .ensure_pr(&FetchSpec {
            server_id: &pr.server_id,
            repo: &pr.repo,
            clone_url,
            pr_ref: &pr_ref,
            number: pr.number,
            base_ref: &pr.base_ref,
            head_sha: &pr.head_sha,
            base_sha: pr.base_sha.as_deref(),
            auth_header: Some(&auth),
        })
        .await?;
    check_cancel(cancel)?;

    let base = svc
        .cache
        .base_commit(&bare_repo, pr.base_sha.as_deref(), &pr.base_ref)
        .await?;
    svc.db.set_review_base(review_id, &base)?;
    let diff = svc.cache.diff(&bare_repo, &base, &pr.head_sha).await?;
    let index = DiffIndex::parse(&diff);
    // The commit list is context only: without a merge base, list from the base instead, and
    // without a list, review without one.
    let merge_base = svc.cache.merge_base(&bare_repo, &base, &pr.head_sha).await;
    let commits = svc
        .cache
        .commit_log(
            &bare_repo,
            merge_base.as_deref().unwrap_or(&base),
            &pr.head_sha,
        )
        .await
        .unwrap_or_default();
    Ok(Fetched {
        bare_repo,
        base,
        diff,
        index,
        commits,
    })
}

/// Writes the review material the reviewer reads from the context dir.
async fn write_context(
    svc: &Service,
    review_id: &str,
    pr: &PullRequest,
    context_dir: &Path,
    fetched: &Fetched,
    tickets: Vec<Ticket>,
) -> AppResult<()> {
    tokio::fs::create_dir_all(context_dir).await?;
    if !tickets.is_empty() {
        tokio::fs::write(context_dir.join("tickets.md"), render::render(&tickets)).await?;
        let refs: Vec<TicketRef> = tickets
            .into_iter()
            .map(|t| TicketRef {
                key: t.key,
                url: t.url,
            })
            .collect();
        svc.db.set_review_tickets(review_id, &refs)?;
        svc.publish_review(review_id);
    }
    tokio::fs::write(context_dir.join("pr.diff"), &fetched.diff).await?;
    tokio::fs::write(context_dir.join("commits.txt"), &fetched.commits).await?;
    tokio::fs::write(
        context_dir.join("description.md"),
        format!("# {}\n\n{}", pr.title, pr.description),
    )
    .await?;
    Ok(())
}

/// For a re-review, writes what changed since the previously reviewed head. Returns whether
/// the file was written; without it the reviewer just reviews the whole diff.
async fn write_incremental_diff(
    svc: &Service,
    bare_repo: &Path,
    reviewed_sha: &str,
    pr: &PullRequest,
    context_dir: &Path,
) -> AppResult<bool> {
    match svc
        .cache
        .diff_between(bare_repo, reviewed_sha, &pr.head_sha)
        .await
    {
        Ok(inc) => {
            tokio::fs::write(context_dir.join("incremental.diff"), inc).await?;
            Ok(true)
        }
        // E.g. after a force push the old head may be gone.
        Err(e) => {
            tracing::debug!("no incremental diff from {reviewed_sha}: {e}");
            Ok(false)
        }
    }
}

/// Packages the configured review skills as a plugin. Returns its dir, if there is one, and
/// what the prompt lists.
async fn prepare_skills(
    svc: &Service,
    review_id: &str,
    pr: &PullRequest,
    review_dir: &Path,
) -> AppResult<(Option<PathBuf>, Vec<SkillInfo>)> {
    let skills = svc.db.skills_for_review(&pr.server_id, &pr.repo)?;
    let (plugin_dir, skill_infos) = build_plugin(&skills, &review_dir.join("plugin"))
        .await?
        .unzip();
    let skill_infos = skill_infos.unwrap_or_default();
    if !skill_infos.is_empty() {
        svc.progress(
            review_id,
            ProgressKind::Status,
            format!("Loaded {} review skill(s)", skill_infos.len()),
        );
    }
    Ok((plugin_dir, skill_infos))
}

/// Runs the reviewer and streams its progress to the review log.
async fn run_reviewer(
    svc: &Service,
    review_id: &str,
    request: RunRequest,
    cancel: CancellationToken,
) -> AppResult<RunOutcome> {
    svc.progress(review_id, ProgressKind::Status, "Reviewer running");
    // Tool details mention absolute paths in the checkout; show them relative to it.
    let worktree = request.cwd.to_string_lossy().into_owned();
    runner::run(request, cancel, |event| match event {
        RunEvent::Started { model, .. } => {
            svc.progress(review_id, ProgressKind::Status, format!("Model: {model}"));
        }
        RunEvent::Tool { name, detail } => {
            let detail = detail.replace(&worktree, ".");
            svc.progress(
                review_id,
                ProgressKind::Tool,
                format!("{name} {detail}").trim().to_string(),
            );
        }
        RunEvent::Text(text) => svc.progress(review_id, ProgressKind::Text, text),
    })
    .await
}

/// Files above this size are not stored; the UI shows only their diff.
const MAX_SNAPSHOT_BYTES: u64 = 512 * 1024;
const MAX_SNAPSHOT_DIFF_LINES: usize = 5000;

/// Copies the critical files at the reviewed head into the review, so the UI can show them
/// after the worktree is gone or the PR has moved on. The paths were already checked against
/// the diff in `postprocess`, and the content comes from the object database, not the checkout.
/// Never fails the review: a file that can't be read is kept with its diff only.
async fn snapshot_files(
    svc: &Service,
    review_id: &str,
    bare_repo: &Path,
    head: &str,
    index: &DiffIndex,
    files: Vec<CriticalFile>,
    cancel: &CancellationToken,
) -> AppResult<Vec<ReviewFile>> {
    let mut out = Vec::with_capacity(files.len());
    for mut meta in files {
        check_cancel(cancel)?;
        let (diff, diff_truncated) = index
            .file(&meta.path)
            .map_or_default(|f| f.view_lines(MAX_SNAPSHOT_DIFF_LINES));
        let read = async {
            Ok::<_, AppError>(
                match svc.cache.tree_entry(bare_repo, head, &meta.path).await? {
                    TreeEntry::Missing => Err(FileNote::Deleted),
                    TreeEntry::NotAFile => Err(FileNote::NotAFile),
                    TreeEntry::Blob { size } if size > MAX_SNAPSHOT_BYTES => {
                        Err(FileNote::TooLarge)
                    }
                    TreeEntry::Blob { .. } => {
                        let text = svc.cache.show_blob(bare_repo, head, &meta.path).await?;
                        if text.contains('\0') {
                            Err(FileNote::Binary)
                        } else {
                            Ok(text)
                        }
                    }
                },
            )
        };
        let content = match read.await {
            Ok(Ok(text)) => Some(text),
            Ok(Err(note)) => {
                meta.note = Some(note);
                None
            }
            Err(e) => {
                svc.progress(
                    review_id,
                    ProgressKind::Error,
                    format!("Could not read {}: {e}", meta.path),
                );
                meta.note = Some(FileNote::Unreadable);
                None
            }
        };
        out.push(ReviewFile {
            meta,
            content: CriticalFileContent {
                content,
                diff,
                diff_truncated,
            },
        });
    }
    Ok(out)
}

/// Loads the tickets linked from the PR, if a ticket source is configured. Never fails the
/// review: problems are reported in the review log and the review runs without them.
async fn load_tickets(
    svc: &Service,
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
    let work_dir = review_dir.join("ticket-fetch");
    for key in keys {
        if cancel.is_cancelled() {
            break;
        }
        svc.progress(
            review_id,
            ProgressKind::Status,
            format!("Loading ticket {key}"),
        );
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
        Err(AppError::Cancelled)
    } else {
        Ok(())
    }
}

/// Notifications and auto-approval once a review is stored.
async fn after_review(
    svc: &Service,
    review_id: &str,
    pr: &PullRequest,
    verdict: Verdict,
    trigger: ReviewTrigger,
    steering: Option<&str>,
) {
    let Ok(settings) = svc.effective_settings(&pr.server_id, Some(&pr.repo)) else {
        return;
    };
    let finding_count = svc.db.list_findings(review_id).map_or(0, |f| f.len());
    let label = format!("{}#{}", pr.repo, pr.number);

    if verdict == Verdict::Approve && settings.auto_approve_on_approve_verdict.value {
        if let Some(excerpt) = steering {
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
    svc: &Service,
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
