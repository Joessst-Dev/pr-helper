//! Shared application state used by commands, the review queue and the watcher.
//! Everything UI-specific goes through [`UiSink`] so the logic is testable without Tauri.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use ts_rs::TS;

use crate::db::models::{
    PullRequest, Review, ReviewStatus, Server, ServerInput, TicketSource, TicketSourceInput,
    TicketSourceKind,
};
use crate::db::{Db, non_empty, now};
use crate::env;
use crate::error::{AppError, AppResult};
use crate::git::repo_cache::RepoCache;
use crate::providers::{GitProvider, make_provider};
use crate::review::queue::ReviewQueue;
use crate::review::user_comment::{self, CommentAdvice, UserCommentInput};
use crate::secrets::SecretStore;
use crate::settings::{EffectiveSettings, SettingsOverrides, resolve};
use crate::tickets::jira::{JiraAuth, JiraClient, parse_extra_fields};
use crate::tickets::{self, Ticket, is_valid_key, mcp, render};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ProgressKind {
    Status,
    Tool,
    Text,
    Error,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProgressEvent {
    pub review_id: String,
    pub kind: ProgressKind,
    pub message: String,
    pub at: String,
}

pub trait UiSink: Send + Sync {
    fn review_progress(&self, event: &ProgressEvent);
    fn review_updated(&self, review: &Review);
    fn tracked_updated(&self);
    fn notify(&self, title: &str, body: &str);
}

/// Builds the provider client for a server and its token. Tests swap in a fake.
pub type ProviderFactory = dyn Fn(&Server, String) -> Arc<dyn GitProvider> + Send + Sync;

const MAX_LOG_LINES: usize = 500;

/// Which tickets a PR links to, per the resolved settings. Nothing is fetched yet.
#[derive(Debug, Clone)]
pub enum TicketPlan {
    /// No ticket source is configured for this server or repo.
    NotConfigured,
    /// A source is configured but nothing can be loaded. `is_error` marks a configuration
    /// problem rather than a PR without ticket keys.
    Skipped {
        source: Option<TicketSource>,
        message: String,
        is_error: bool,
    },
    Ready {
        source: TicketSource,
        keys: Vec<String>,
    },
}

fn validate_request_id(id: &str) -> AppResult<()> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(AppError::invalid("invalid request id"))
    }
}

/// One ticket loaded for display, or why it could not be loaded.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TicketLoad {
    pub key: String,
    pub ticket: Option<Ticket>,
    pub error: Option<String>,
}

pub struct Service {
    pub db: Arc<Db>,
    pub secrets: Arc<dyn SecretStore>,
    pub cache: Arc<RepoCache>,
    pub sink: Arc<dyn UiSink>,
    pub queue: ReviewQueue,
    provider_factory: Box<ProviderFactory>,
    logs: Mutex<HashMap<String, VecDeque<ProgressEvent>>>,
    /// Running ticket loads of the PR view, so the UI can cancel them.
    ticket_loads: CancelRegistry,
    /// Running AI checks of user comments, so the UI can cancel them.
    comment_checks: CancelRegistry,
}

/// At most this many cancels for requests that have not started are kept.
const MAX_STRAY_CANCELS: usize = 64;

/// Cancellation tokens of running UI requests, by request id.
#[derive(Default)]
struct CancelRegistry(Mutex<HashMap<String, CancellationToken>>);

impl CancelRegistry {
    fn tokens(&self) -> MutexGuard<'_, HashMap<String, CancellationToken>> {
        // A poisoned map is still usable, and the guard's drop must not panic.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers a request until the returned guard drops. A cancel can arrive before the
    /// request starts; it leaves a cancelled token behind that the request picks up here.
    fn start(&self, id: &str) -> (CancellationToken, CancelGuard<'_>) {
        let token = self.tokens().entry(id.to_string()).or_default().clone();
        let guard = CancelGuard {
            registry: self,
            id: id.to_string(),
        };
        (token, guard)
    }

    /// Cancels a request. Returns whether it was running.
    fn cancel(&self, id: &str) -> bool {
        let mut tokens = self.tokens();
        let running = tokens.contains_key(id);
        tokens.entry(id.to_string()).or_default().cancel();
        if !running && tokens.len() > MAX_STRAY_CANCELS {
            tokens.retain(|_, t| !t.is_cancelled());
        }
        running
    }
}

/// Unregisters a request when it ends, also when the command future is dropped.
struct CancelGuard<'a> {
    registry: &'a CancelRegistry,
    id: String,
}

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        self.registry.tokens().remove(&self.id);
    }
}

impl Service {
    pub fn new(
        db: Arc<Db>,
        secrets: Arc<dyn SecretStore>,
        cache_root: PathBuf,
        sink: Arc<dyn UiSink>,
        provider_factory: Option<Box<ProviderFactory>>,
    ) -> AppResult<Arc<Self>> {
        let config = db.app_config()?;
        let git = env::resolve_tool("git", config.git_path.as_deref()).unwrap_or_else(|e| {
            tracing::warn!("using git from PATH: {e}");
            PathBuf::from("git")
        });
        Ok(Arc::new(Self {
            cache: Arc::new(RepoCache::new(
                cache_root,
                git,
                env::login_path().to_string(),
            )),
            queue: ReviewQueue::new(config.max_concurrent_reviews.max(1) as usize),
            db,
            secrets,
            sink,
            provider_factory: provider_factory.unwrap_or_else(|| Box::new(make_provider)),
            logs: Mutex::new(HashMap::new()),
            ticket_loads: CancelRegistry::default(),
            comment_checks: CancelRegistry::default(),
        }))
    }

    pub fn provider(&self, server: &Server) -> AppResult<Arc<dyn GitProvider>> {
        let token = self.secrets.require(&server.id)?;
        Ok((self.provider_factory)(server, token))
    }

    pub fn provider_for(&self, server_id: &str) -> AppResult<(Server, Arc<dyn GitProvider>)> {
        let server = self.db.get_server(server_id)?;
        let provider = self.provider(&server)?;
        Ok((server, provider))
    }

    /// Verifies the token against the server, then stores the server in the database and the
    /// token in the secret store (the OS keychain). The token is never written to the database.
    pub async fn save_server(&self, id: Option<&str>, input: &ServerInput) -> AppResult<Server> {
        if input.name.trim().is_empty() || input.base_url.trim().is_empty() {
            return Err(AppError::invalid("name and URL are required"));
        }
        let base_url = crate::db::normalize_base_url(&input.base_url);
        let new_token = non_empty(input.token.as_deref());
        let existing = id.map(|id| self.db.get_server(id)).transpose()?;
        let token = match (new_token, &existing) {
            (Some(t), _) => t.to_string(),
            // A stored token is only reused for the host it was issued for; sending it to a
            // newly typed URL would leak it to whatever that URL points at.
            (None, Some(old)) if old.base_url != base_url || old.kind != input.kind => {
                return Err(AppError::invalid(
                    "enter the access token again when changing the server URL or type",
                ));
            }
            (None, Some(old)) => self.secrets.require(&old.id)?,
            (None, None) => return Err(AppError::invalid("an access token is required")),
        };
        let probe = Server {
            id: id.unwrap_or_default().to_string(),
            kind: input.kind,
            name: input.name.clone(),
            base_url,
            username: None,
            pr_scope: input.pr_scope.clone(),
            settings: SettingsOverrides::default(),
            created_at: String::new(),
        };
        let username = (self.provider_factory)(&probe, token.clone())
            .current_user()
            .await?;

        let server = match &existing {
            Some(old) => self
                .update_with_token(&old.id, new_token, || self.db.update_server(&old.id, input))?,
            None => {
                let server = self.db.insert_server(input)?;
                if let Err(e) = self.secrets.set(&server.id, &token) {
                    // Never keep a server whose token could not be stored securely.
                    if let Err(undo) = self.db.delete_server(&server.id) {
                        tracing::error!(
                            "could not remove server {} after a keychain failure: {undo}",
                            server.id
                        );
                    }
                    return Err(e);
                }
                server
            }
        };
        self.db.set_server_username(&server.id, &username)?;
        self.db.get_server(&server.id)
    }

    /// Keychain account for a ticket source's token. Prefixed so it never collides with a
    /// server id.
    pub fn ticket_secret_id(source_id: &str) -> String {
        format!("ticket-source:{source_id}")
    }

    fn jira_client(source: &TicketSource, token: String) -> AppResult<JiraClient> {
        let auth = match source.kind {
            TicketSourceKind::JiraCloud => JiraAuth::Basic {
                email: non_empty(source.email.as_deref())
                    .ok_or_else(|| AppError::invalid("Jira Cloud needs the account email"))?
                    .to_string(),
                token,
            },
            TicketSourceKind::JiraServer => JiraAuth::Bearer(token),
            TicketSourceKind::Mcp => {
                return Err(AppError::invalid("MCP ticket sources have no REST client"));
            }
        };
        Ok(JiraClient::new(
            &source.base_url,
            auth,
            crate::providers::http_client(),
        ))
    }

    /// Updates an existing row with `update`. A new token is written first, so a failed keychain
    /// write can never leave a changed URL paired with the old token. If `update` then fails,
    /// the previous token is put back.
    fn update_with_token<T>(
        &self,
        account: &str,
        new_token: Option<&str>,
        update: impl FnOnce() -> AppResult<T>,
    ) -> AppResult<T> {
        let previous = match new_token {
            Some(t) => {
                let previous = self.secrets.get(account)?;
                self.secrets.set(account, t)?;
                previous
            }
            None => None,
        };
        update().inspect_err(|_| {
            if let Some(previous) = previous
                && let Err(e) = self.secrets.set(account, &previous)
            {
                tracing::error!("could not restore the previous token of {account}: {e}");
            }
        })
    }

    /// Like [`Self::save_server`]: verifies the token with Jira, stores it only in the secret
    /// store (keychain) and never in the database. MCP sources have no token.
    pub async fn save_ticket_source(
        &self,
        id: Option<&str>,
        input: &TicketSourceInput,
    ) -> AppResult<TicketSource> {
        if input.name.trim().is_empty() {
            return Err(AppError::invalid("a name is required"));
        }
        let base_url = crate::db::normalize_base_url(&input.base_url);
        let existing = id.map(|id| self.db.get_ticket_source(id)).transpose()?;
        let probe = TicketSource {
            id: id.unwrap_or_default().to_string(),
            kind: input.kind,
            name: input.name.clone(),
            base_url: base_url.clone(),
            email: input.email.clone(),
            mcp_server: input.mcp_server.clone(),
            mcp_tools: input.mcp_tools.clone(),
            model: input.model.clone(),
            extra_fields: input.extra_fields.clone(),
            username: None,
            created_at: String::new(),
        };

        if !input.kind.needs_token() {
            mcp::allowed_tools(&probe)?;
            let saved = self.db.upsert_ticket_source(id, input, None)?;
            // A source switched to MCP must not keep an old Jira token around.
            if existing.is_some_and(|old| old.kind.needs_token()) {
                self.secrets.delete(&Self::ticket_secret_id(&saved.id))?;
            }
            return Ok(saved);
        }

        if !base_url.starts_with("https://") && !base_url.starts_with("http://") {
            return Err(AppError::invalid(
                "enter the Jira URL, e.g. https://acme.atlassian.net",
            ));
        }
        let new_token = non_empty(input.token.as_deref());
        let token = match (new_token, &existing) {
            (Some(t), _) => t.to_string(),
            // Never send a stored token to a newly typed URL.
            (None, Some(old)) if old.base_url != base_url || old.kind != input.kind => {
                return Err(AppError::invalid(
                    "enter the API token again when changing the Jira URL or type",
                ));
            }
            (None, Some(old)) => self.secrets.require(&Self::ticket_secret_id(&old.id))?,
            (None, None) => return Err(AppError::invalid("an API token is required")),
        };
        let username = Self::jira_client(&probe, token.clone())?.myself().await?;

        match &existing {
            Some(old) => {
                self.update_with_token(&Self::ticket_secret_id(&old.id), new_token, || {
                    self.db
                        .upsert_ticket_source(Some(&old.id), input, Some(&username))
                })
            }
            None => {
                let saved = self.db.upsert_ticket_source(None, input, Some(&username))?;
                if let Err(e) = self.secrets.set(&Self::ticket_secret_id(&saved.id), &token) {
                    if let Err(undo) = self.db.delete_ticket_source(&saved.id) {
                        tracing::error!(
                            "could not remove ticket source {} after a keychain failure: {undo}",
                            saved.id
                        );
                    }
                    return Err(e);
                }
                Ok(saved)
            }
        }
    }

    pub fn delete_ticket_source(&self, id: &str) -> AppResult<()> {
        // Token first, so a refusing keychain leaves the source visible for a retry.
        self.secrets.delete(&Self::ticket_secret_id(id))?;
        self.db.delete_ticket_source(id)
    }

    /// Loads one ticket. `work_dir` is a scratch directory for the MCP fetcher.
    pub async fn fetch_ticket(
        &self,
        source: &TicketSource,
        key: &str,
        work_dir: &std::path::Path,
        cancel: CancellationToken,
    ) -> AppResult<Ticket> {
        if !is_valid_key(key) {
            return Err(AppError::invalid(format!("invalid ticket key {key:?}")));
        }
        let extra = parse_extra_fields(source.extra_fields.as_deref());
        match source.kind {
            TicketSourceKind::Mcp => {
                let config = self.db.app_config()?;
                let claude = env::resolve_tool("claude", config.claude_path.as_deref())?;
                mcp::fetch(
                    source,
                    key,
                    &extra,
                    mcp::FetchEnv {
                        claude,
                        path_env: env::login_path().to_string(),
                        work_dir,
                        cancel,
                    },
                )
                .await
            }
            _ => {
                let token = self.secrets.require(&Self::ticket_secret_id(&source.id))?;
                let client = Self::jira_client(source, token)?;
                tokio::select! {
                    t = client.issue(key, &extra) => t,
                    _ = cancel.cancelled() => Err(AppError::Cancelled),
                }
            }
        }
    }

    /// Loads a ticket with a saved source and returns what the reviewer would see.
    pub async fn test_ticket_source(&self, id: &str, key: &str) -> AppResult<String> {
        let source = self.db.get_ticket_source(id)?;
        let key = key.trim().to_ascii_uppercase();
        let work_dir = self
            .cache
            .review_dir(&format!("ticket-test-{}", crate::db::new_id()));
        let result = self
            .fetch_ticket(&source, &key, &work_dir, CancellationToken::new())
            .await;
        let _ = tokio::fs::remove_dir_all(&work_dir).await;
        Ok(render::render(&[result?]))
    }

    /// The ticket source and allow-listed ticket keys for a PR.
    pub fn ticket_plan(&self, settings: &EffectiveSettings, pr: &PullRequest) -> TicketPlan {
        let Some(source_id) = settings.ticket_source_id.value.as_deref() else {
            return TicketPlan::NotConfigured;
        };
        let source = match self.db.get_ticket_source(source_id) {
            Ok(s) => s,
            Err(e) => {
                return TicketPlan::Skipped {
                    source: None,
                    message: format!("Tickets skipped: {e}"),
                    is_error: true,
                };
            }
        };
        let projects = tickets::parse_project_keys(&settings.ticket_project_keys.value);
        if projects.is_empty() {
            return TicketPlan::Skipped {
                source: Some(source),
                message: "Tickets skipped: no Jira project keys are configured".into(),
                is_error: true,
            };
        }
        let keys = tickets::extract_keys(&pr.head_ref, &pr.title, &pr.description, &projects);
        if keys.is_empty() {
            return TicketPlan::Skipped {
                source: Some(source),
                message: format!(
                    "No ticket key for {} found in the branch, title or description",
                    projects.join(", ")
                ),
                is_error: false,
            };
        }
        TicketPlan::Ready { source, keys }
    }

    /// Loads tickets for display in the PR view. The keys come from the UI, so they are
    /// validated and checked against the allowed projects of the server or repo again.
    /// A ticket that fails to load is reported in its entry and never fails the call.
    pub async fn load_pr_tickets(
        &self,
        server_id: &str,
        repo: &str,
        keys: &[String],
        request_id: &str,
    ) -> AppResult<Vec<TicketLoad>> {
        validate_request_id(request_id)?;
        let (cancel, _guard) = self.ticket_loads.start(request_id);

        let settings = self.effective_settings(server_id, Some(repo))?;
        let Some(source_id) = settings.ticket_source_id.value.as_deref() else {
            return Err(AppError::invalid(
                "no ticket source is configured for this repository",
            ));
        };
        let source = self.db.get_ticket_source(source_id)?;
        let projects = tickets::parse_project_keys(&settings.ticket_project_keys.value);
        let keys = tickets::filter_keys(keys, &projects);
        let mut loaded = Vec::with_capacity(keys.len());
        for key in keys {
            if cancel.is_cancelled() {
                return Err(AppError::Cancelled);
            }
            let work_dir = self
                .cache
                .review_dir(&format!("ticket-view-{}", crate::db::new_id()));
            let result = self
                .fetch_ticket(&source, &key, &work_dir, cancel.clone())
                .await;
            let _ = tokio::fs::remove_dir_all(&work_dir).await;
            if cancel.is_cancelled() {
                return Err(AppError::Cancelled);
            }
            loaded.push(match result {
                Ok(t) => TicketLoad {
                    key,
                    ticket: Some(t),
                    error: None,
                },
                Err(e) => TicketLoad {
                    key,
                    ticket: None,
                    error: Some(e.to_string()),
                },
            });
        }
        Ok(loaded)
    }

    /// An image attachment of a ticket as a `data:` URL, fetched with the Jira source that is
    /// configured for the server or repo. Like [`Self::load_pr_tickets`], the ticket must be in
    /// an allowed project, and the attachment must belong to it. The webview never sees the
    /// credentials.
    pub async fn load_ticket_image(
        &self,
        server_id: &str,
        repo: &str,
        key: &str,
        attachment_id: &str,
    ) -> AppResult<String> {
        use base64::Engine;
        let settings = self.effective_settings(server_id, Some(repo))?;
        let Some(source_id) = settings.ticket_source_id.value.as_deref() else {
            return Err(AppError::invalid(
                "no ticket source is configured for this repository",
            ));
        };
        let projects = tickets::parse_project_keys(&settings.ticket_project_keys.value);
        let Some(key) = tickets::filter_keys(&[key.to_string()], &projects).pop() else {
            return Err(AppError::invalid(format!(
                "ticket {key:?} is not in an allowed project"
            )));
        };
        let source = self.db.get_ticket_source(source_id)?;
        let token = match source.kind {
            TicketSourceKind::Mcp => {
                return Err(AppError::invalid(
                    "images can only be loaded from a Jira REST source",
                ));
            }
            _ => self.secrets.require(&Self::ticket_secret_id(&source.id))?,
        };
        let (mime, bytes) = Self::jira_client(&source, token)?
            .attachment_image(&key, attachment_id)
            .await?;
        Ok(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    }

    /// Cancels a running ticket load of the PR view, including an MCP fetcher run.
    /// Returns whether a load with this id was running.
    pub fn cancel_ticket_load(&self, request_id: &str) -> AppResult<bool> {
        validate_request_id(request_id)?;
        Ok(self.ticket_loads.cancel(request_id))
    }

    /// Runs the AI check of a user comment; `cancel_comment_check` stops it.
    pub async fn check_user_comment(
        &self,
        input: &UserCommentInput,
        request_id: &str,
    ) -> AppResult<CommentAdvice> {
        validate_request_id(request_id)?;
        let (cancel, _guard) = self.comment_checks.start(request_id);
        user_comment::check(self, input, cancel).await
    }

    /// Returns whether a check with this id was running.
    pub fn cancel_comment_check(&self, request_id: &str) -> AppResult<bool> {
        validate_request_id(request_id)?;
        Ok(self.comment_checks.cancel(request_id))
    }

    pub fn effective_settings(
        &self,
        server_id: &str,
        repo: Option<&str>,
    ) -> AppResult<EffectiveSettings> {
        let global = self.db.global_settings()?;
        let server = self.db.get_server(server_id)?;
        let repo_settings = match repo {
            Some(r) => self.db.repo_settings(server_id, r)?,
            None => None,
        };
        Ok(resolve(&global, &server.settings, repo_settings.as_ref()))
    }

    pub fn progress(&self, review_id: &str, kind: ProgressKind, message: impl Into<String>) {
        let event = ProgressEvent {
            review_id: review_id.to_string(),
            kind,
            message: message.into(),
            at: now(),
        };
        {
            let mut logs = self.logs.lock().unwrap_or_else(PoisonError::into_inner);
            let log = logs.entry(review_id.to_string()).or_default();
            if log.len() >= MAX_LOG_LINES {
                log.pop_front();
            }
            log.push_back(event.clone());
        }
        self.sink.review_progress(&event);
    }

    pub fn progress_log(&self, review_id: &str) -> Vec<ProgressEvent> {
        self.logs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(review_id)
            .map_or_default(|log| log.iter().cloned().collect())
    }

    pub fn publish_review(&self, review_id: &str) {
        match self.db.get_review(review_id) {
            Ok(review) => self.sink.review_updated(&review),
            Err(e) => tracing::warn!("could not load review {review_id} to publish it: {e}"),
        }
    }

    /// Records a review's status. Callers treat this as best effort (the review itself already
    /// succeeded or failed), so a failed write is logged instead of returned.
    pub fn record_review_status(
        &self,
        review_id: &str,
        status: ReviewStatus,
        message: Option<&str>,
    ) {
        if let Err(e) = self.db.set_review_status(review_id, status, message) {
            tracing::warn!(
                "could not mark review {review_id} as {}: {e}",
                status.as_str()
            );
        }
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;

    #[derive(Default)]
    pub struct RecordingSink {
        pub notifications: Mutex<Vec<(String, String)>>,
        pub updates: Mutex<Vec<Review>>,
    }

    impl UiSink for RecordingSink {
        fn review_progress(&self, _event: &ProgressEvent) {}
        fn review_updated(&self, review: &Review) {
            self.updates.lock().unwrap().push(review.clone());
        }
        fn tracked_updated(&self) {}
        fn notify(&self, title: &str, body: &str) {
            self.notifications
                .lock()
                .unwrap()
                .push((title.into(), body.into()));
        }
    }
}
