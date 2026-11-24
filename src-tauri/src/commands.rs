//! The command surface for the frontend. Thin wrappers over `Service` and the review modules.

use std::sync::Arc;

use serde::Serialize;
use tauri::State;
use ts_rs::TS;

use crate::db::models::*;
use crate::env;
use crate::error::{AppError, AppResult};
use crate::providers::PrFilter;
use crate::review::post::{self, PostOutcome, PostRequest};
use crate::review::postprocess::{CommentCheck, check_comment};
use crate::review::queue;
use crate::review::skills::validate_skill_dir;
use crate::review::user_comment::{self, CommentAdvice, UserCommentInput};
use crate::service::{ProgressEvent, Service, TicketLoad, TicketPlan};
use crate::settings::{AppConfig, EffectiveSettings, SettingsOverrides};

type Svc<'a> = State<'a, Arc<Service>>;

// ---- servers -------------------------------------------------------------

#[tauri::command]
pub fn list_servers(svc: Svc<'_>) -> AppResult<Vec<Server>> {
    svc.db.list_servers()
}

/// Verifies the connection before saving. Returns the saved server with its username.
#[tauri::command]
pub async fn save_server(
    svc: Svc<'_>,
    id: Option<String>,
    input: ServerInput,
) -> AppResult<Server> {
    svc.save_server(id.as_deref(), &input).await
}

// Keychain access can block on a macOS permission prompt, so this runs off the main thread.
#[tauri::command(async)]
pub fn delete_server(svc: Svc<'_>, id: String) -> AppResult<()> {
    // Token first: if the keychain refuses, the server stays visible so it can be retried
    // instead of leaving an orphaned token behind.
    svc.secrets.delete(&id)?;
    svc.queue.cancel_server(&id);
    svc.db.delete_server(&id)?;
    svc.cache.remove_server(&id);
    svc.sink.tracked_updated();
    Ok(())
}

// ---- ticket sources --------------------------------------------------------

#[tauri::command]
pub fn list_ticket_sources(svc: Svc<'_>) -> AppResult<Vec<TicketSource>> {
    svc.db.list_ticket_sources()
}

/// Verifies Jira credentials before saving; the token goes to the keychain only.
#[tauri::command]
pub async fn save_ticket_source(
    svc: Svc<'_>,
    id: Option<String>,
    input: TicketSourceInput,
) -> AppResult<TicketSource> {
    svc.save_ticket_source(id.as_deref(), &input).await
}

// Keychain access can block on a macOS permission prompt.
#[tauri::command(async)]
pub fn delete_ticket_source(svc: Svc<'_>, id: String) -> AppResult<()> {
    svc.delete_ticket_source(&id)
}

/// Loads one ticket and returns the markdown the reviewer would get.
#[tauri::command]
pub async fn test_ticket_source(svc: Svc<'_>, id: String, key: String) -> AppResult<String> {
    svc.test_ticket_source(&id, &key).await
}

#[tauri::command]
pub fn save_server_settings(
    svc: Svc<'_>,
    id: String,
    settings: SettingsOverrides,
) -> AppResult<()> {
    svc.db.set_server_settings(&id, &settings)
}

// ---- pull requests -------------------------------------------------------

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PrListItem {
    pub pr: PullRequest,
    pub tracked: Option<TrackedPr>,
    pub latest_review: Option<Review>,
}

#[tauri::command]
pub async fn list_prs(
    svc: Svc<'_>,
    server_id: String,
    filter: PrFilter,
) -> AppResult<Vec<PrListItem>> {
    let (_server, provider) = svc.provider_for(&server_id)?;
    let prs = provider.list_open_prs(filter).await?;
    prs.into_iter()
        .map(|pr| {
            let tracked = svc.db.get_tracked(&server_id, &pr.repo, pr.number)?;
            let latest_review = svc
                .db
                .list_reviews_for_pr(&server_id, &pr.repo, pr.number)?
                .into_iter()
                .next();
            Ok(PrListItem {
                pr,
                tracked,
                latest_review,
            })
        })
        .collect()
}

/// Linked tickets of a PR as far as they are known without fetching them.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PrTicketInfo {
    pub source_name: String,
    pub source_kind: Option<TicketSourceKind>,
    pub keys: Vec<String>,
    /// Why no tickets can be loaded, if so.
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PrDetail {
    pub pr: PullRequest,
    pub tracked: Option<TrackedPr>,
    pub reviews: Vec<Review>,
    /// `None` when no ticket source is configured for the server or repo.
    pub tickets: Option<PrTicketInfo>,
}

#[tauri::command]
pub async fn get_pr_detail(
    svc: Svc<'_>,
    server_id: String,
    project_id: String,
    number: u64,
) -> AppResult<PrDetail> {
    let (_server, provider) = svc.provider_for(&server_id)?;
    let pr = provider.get_pr(&project_id, number).await?;
    let settings = svc.effective_settings(&server_id, Some(&pr.repo))?;
    let tickets = match svc.ticket_plan(&settings, &pr) {
        TicketPlan::NotConfigured => None,
        TicketPlan::Skipped {
            source, message, ..
        } => Some(PrTicketInfo {
            source_name: source.as_ref().map_or_default(|s| s.name.clone()),
            source_kind: source.map(|s| s.kind),
            keys: Vec::new(),
            message: Some(message),
        }),
        TicketPlan::Ready { source, keys } => Some(PrTicketInfo {
            source_name: source.name,
            source_kind: Some(source.kind),
            keys,
            message: None,
        }),
    };
    Ok(PrDetail {
        tracked: svc.db.get_tracked(&server_id, &pr.repo, pr.number)?,
        reviews: svc
            .db
            .list_reviews_for_pr(&server_id, &pr.repo, pr.number)?,
        tickets,
        pr,
    })
}

#[tauri::command]
pub async fn load_pr_tickets(
    svc: Svc<'_>,
    server_id: String,
    repo: String,
    keys: Vec<String>,
    request_id: String,
) -> AppResult<Vec<TicketLoad>> {
    svc.load_pr_tickets(&server_id, &repo, &keys, &request_id)
        .await
}

#[tauri::command]
pub async fn load_ticket_image(
    svc: Svc<'_>,
    server_id: String,
    repo: String,
    key: String,
    attachment_id: String,
) -> AppResult<String> {
    svc.load_ticket_image(&server_id, &repo, &key, &attachment_id)
        .await
}

#[tauri::command]
pub fn cancel_ticket_load(svc: Svc<'_>, request_id: String) -> AppResult<bool> {
    svc.cancel_ticket_load(&request_id)
}

#[tauri::command]
pub async fn start_review(
    svc: Svc<'_>,
    server_id: String,
    project_id: String,
    number: u64,
) -> AppResult<Review> {
    let (_server, provider) = svc.provider_for(&server_id)?;
    let pr = provider.get_pr(&project_id, number).await?;
    if pr.state != PrState::Open {
        return Err(AppError::invalid("this pull request is no longer open"));
    }
    queue::enqueue(&svc, pr, ReviewTrigger::Manual)
}

/// The stored snapshot of one critical file of a review.
#[tauri::command]
pub fn get_review_file(
    svc: Svc<'_>,
    review_id: String,
    path: String,
) -> AppResult<CriticalFileContent> {
    svc.db.get_review_file(&review_id, &path)
}

#[tauri::command]
pub fn cancel_review(svc: Svc<'_>, review_id: String) -> bool {
    svc.queue.cancel(&review_id)
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ReviewDetail {
    pub review: Review,
    pub findings: Vec<Finding>,
    /// Files the reviewer recommends reading by hand; their code comes from `get_review_file`.
    pub critical_files: Vec<CriticalFile>,
    pub settings: EffectiveSettings,
    pub server: Server,
    pub log: Vec<ProgressEvent>,
    pub web_url: Option<String>,
}

#[tauri::command]
pub fn get_review(svc: Svc<'_>, review_id: String) -> AppResult<ReviewDetail> {
    let review = svc.db.get_review(&review_id)?;
    let server = svc.db.get_server(&review.server_id)?;
    let web_url = svc
        .db
        .get_tracked(&review.server_id, &review.repo, review.number)?
        .map(|t| t.web_url);
    Ok(ReviewDetail {
        findings: svc.db.list_findings(&review.id)?,
        critical_files: svc.db.list_review_files(&review.id)?,
        settings: svc.effective_settings(&review.server_id, Some(&review.repo))?,
        log: svc.progress_log(&review.id),
        server,
        web_url,
        review,
    })
}

#[tauri::command]
pub fn list_reviews_for_pr(
    svc: Svc<'_>,
    server_id: String,
    repo: String,
    number: u64,
) -> AppResult<Vec<Review>> {
    svc.db.list_reviews_for_pr(&server_id, &repo, number)
}

#[tauri::command]
pub fn list_recent_reviews(svc: Svc<'_>, limit: u32) -> AppResult<Vec<Review>> {
    svc.db.list_recent_reviews(limit)
}

#[tauri::command]
pub fn update_finding(
    svc: Svc<'_>,
    id: String,
    selected: bool,
    proposed_comment: String,
) -> AppResult<Finding> {
    user_comment::update_finding(&svc, &id, selected, &proposed_comment)
}

/// Adds a comment the user wrote on lines of a critical file. It is posted like a finding.
#[tauri::command]
pub fn add_user_comment(svc: Svc<'_>, input: UserCommentInput) -> AppResult<Finding> {
    user_comment::add(&svc, &input)
}

#[tauri::command]
pub fn delete_user_comment(svc: Svc<'_>, id: String) -> AppResult<()> {
    user_comment::delete(&svc, &id)
}

/// Asks the AI whether a (possibly unsaved) user comment holds and how to word it.
#[tauri::command]
pub async fn check_user_comment(
    svc: Svc<'_>,
    input: UserCommentInput,
    request_id: String,
) -> AppResult<CommentAdvice> {
    svc.check_user_comment(&input, &request_id).await
}

#[tauri::command]
pub fn cancel_comment_check(svc: Svc<'_>, request_id: String) -> AppResult<bool> {
    svc.cancel_comment_check(&request_id)
}

#[tauri::command]
pub fn check_comment_text(
    svc: Svc<'_>,
    server_id: String,
    repo: String,
    text: String,
) -> AppResult<CommentCheck> {
    let settings = svc.effective_settings(&server_id, Some(&repo))?;
    Ok(check_comment(&text, settings.forbid_ai_attribution.value))
}

#[tauri::command]
pub async fn post_findings(svc: Svc<'_>, request: PostRequest) -> AppResult<PostOutcome> {
    post::post_findings(&svc, request).await
}

/// Shell command to continue the reviewer session interactively (needs `keepWorktrees`).
#[tauri::command]
pub fn resume_command(svc: Svc<'_>, review_id: String) -> AppResult<String> {
    let review = svc.db.get_review(&review_id)?;
    let session = review
        .session_id
        .ok_or_else(|| AppError::invalid("this review has no session"))?;
    let dir = review
        .worktree_path
        .filter(|p| std::path::Path::new(p).is_dir())
        .ok_or_else(|| {
            AppError::invalid(
                "the worktree was cleaned up; enable \"Keep worktrees\" in Settings to resume sessions",
            )
        })?;
    Ok(format!(
        "cd '{}' && claude --resume {session}",
        dir.replace('\'', "'\\''")
    ))
}

// ---- tracking ------------------------------------------------------------

#[tauri::command]
pub fn list_tracked(svc: Svc<'_>) -> AppResult<Vec<TrackedPr>> {
    svc.db.list_tracked(true)
}

#[tauri::command]
pub fn untrack_pr(svc: Svc<'_>, server_id: String, repo: String, number: u64) -> AppResult<()> {
    svc.db.untrack(&server_id, &repo, number)?;
    svc.sink.tracked_updated();
    Ok(())
}

// ---- settings ------------------------------------------------------------

#[tauri::command]
pub fn get_global_settings(svc: Svc<'_>) -> AppResult<SettingsOverrides> {
    svc.db.global_settings()
}

#[tauri::command]
pub fn save_global_settings(svc: Svc<'_>, settings: SettingsOverrides) -> AppResult<()> {
    svc.db.set_global_settings(&settings)
}

#[tauri::command]
pub fn get_repo_settings(
    svc: Svc<'_>,
    server_id: String,
    repo: String,
) -> AppResult<SettingsOverrides> {
    Ok(svc.db.repo_settings(&server_id, &repo)?.unwrap_or_default())
}

#[tauri::command]
pub fn save_repo_settings(
    svc: Svc<'_>,
    server_id: String,
    repo: String,
    settings: SettingsOverrides,
) -> AppResult<()> {
    if repo.trim().is_empty() {
        return Err(AppError::invalid("repository path is required"));
    }
    svc.db.set_repo_settings(&server_id, repo.trim(), &settings)
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RepoSettingsEntry {
    pub repo: String,
    pub settings: SettingsOverrides,
}

#[tauri::command]
pub fn list_repo_settings(svc: Svc<'_>, server_id: String) -> AppResult<Vec<RepoSettingsEntry>> {
    Ok(svc
        .db
        .list_repo_settings(&server_id)?
        .into_iter()
        .map(|(repo, settings)| RepoSettingsEntry { repo, settings })
        .collect())
}

#[tauri::command]
pub fn effective_settings(
    svc: Svc<'_>,
    server_id: String,
    repo: Option<String>,
) -> AppResult<EffectiveSettings> {
    svc.effective_settings(&server_id, repo.as_deref())
}

/// What a level would inherit if it set nothing itself: defaults for global, global for a
/// server (`repo` = None), global + server for a repo.
#[tauri::command]
pub fn inherited_settings(
    svc: Svc<'_>,
    server_id: Option<String>,
    repo: Option<String>,
) -> AppResult<EffectiveSettings> {
    let empty = SettingsOverrides::default();
    Ok(match (server_id, repo) {
        (None, _) => crate::settings::resolve(&empty, &empty, None),
        (Some(_), None) => crate::settings::resolve(&svc.db.global_settings()?, &empty, None),
        (Some(id), Some(_)) => {
            let server = svc.db.get_server(&id)?;
            crate::settings::resolve(&svc.db.global_settings()?, &server.settings, None)
        }
    })
}

#[tauri::command]
pub fn get_app_config(svc: Svc<'_>) -> AppResult<AppConfig> {
    svc.db.app_config()
}

#[tauri::command(async)]
pub fn save_app_config(app: tauri::AppHandle, svc: Svc<'_>, config: AppConfig) -> AppResult<()> {
    use tauri_plugin_autostart::ManagerExt;
    let autolaunch = app.autolaunch();
    let result = if config.launch_at_login {
        autolaunch.enable()
    } else {
        autolaunch.disable()
    };
    result.map_err(|e| AppError::invalid(format!("could not update launch at login: {e}")))?;
    if let Ok(git) = env::resolve_tool("git", config.git_path.as_deref()) {
        svc.cache.set_git(git);
    }
    svc.queue
        .set_limit(config.max_concurrent_reviews.max(1) as usize);
    svc.db.set_app_config(&config)
}

// ---- skills --------------------------------------------------------------

#[tauri::command]
pub fn list_skills(svc: Svc<'_>, server_id: String) -> AppResult<Vec<Skill>> {
    svc.db.list_skills(&server_id)
}

#[tauri::command(async)]
pub fn save_skill(svc: Svc<'_>, id: Option<String>, input: SkillInput) -> AppResult<Skill> {
    if input.name.trim().is_empty() {
        return Err(AppError::invalid("skill name is required"));
    }
    match (&input.path, &input.content) {
        (Some(p), _) if !p.trim().is_empty() => validate_skill_dir(std::path::Path::new(p.trim()))?,
        (_, Some(c)) if !c.trim().is_empty() => {}
        _ => {
            return Err(AppError::invalid(
                "choose a skill folder or write the skill content",
            ));
        }
    }
    svc.db.upsert_skill(id.as_deref(), &input)
}

#[tauri::command]
pub fn delete_skill(svc: Svc<'_>, id: String) -> AppResult<()> {
    svc.db.delete_skill(&id)
}

// ---- environment ---------------------------------------------------------

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ToolStatus {
    pub path: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EnvironmentStatus {
    pub claude: ToolStatus,
    pub git: ToolStatus,
}

async fn tool_status(name: &str, override_path: Option<&str>) -> ToolStatus {
    match env::resolve_tool(name, override_path) {
        Ok(path) => {
            let out = tokio::process::Command::new(&path)
                .arg("--version")
                .env("PATH", env::login_path())
                .output()
                .await;
            match out {
                Ok(o) if o.status.success() => ToolStatus {
                    path: Some(path.to_string_lossy().into()),
                    version: Some(String::from_utf8_lossy(&o.stdout).trim().to_string()),
                    error: None,
                },
                Ok(o) => ToolStatus {
                    path: Some(path.to_string_lossy().into()),
                    version: None,
                    error: Some(String::from_utf8_lossy(&o.stderr).trim().to_string()),
                },
                Err(e) => ToolStatus {
                    path: Some(path.to_string_lossy().into()),
                    version: None,
                    error: Some(e.to_string()),
                },
            }
        }
        Err(e) => ToolStatus {
            path: None,
            version: None,
            error: Some(e.to_string()),
        },
    }
}

#[tauri::command]
pub async fn check_environment(svc: Svc<'_>) -> AppResult<EnvironmentStatus> {
    let config = svc.db.app_config()?;
    Ok(EnvironmentStatus {
        claude: tool_status("claude", config.claude_path.as_deref()).await,
        git: tool_status("git", config.git_path.as_deref()).await,
    })
}
