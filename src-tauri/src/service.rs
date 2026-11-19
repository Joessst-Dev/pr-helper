//! Shared application state used by commands, the review queue and the watcher.
//! Everything UI-specific goes through [`UiSink`] so the logic is testable without Tauri.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use ts_rs::TS;

use crate::db::models::{Review, Server, ServerInput};
use crate::db::{Db, now};
use crate::env;
use crate::error::{AppError, AppResult};
use crate::git::repo_cache::RepoCache;
use crate::providers::{GitProvider, make_provider};
use crate::review::queue::ReviewQueue;
use crate::secrets::SecretStore;
use crate::settings::{EffectiveSettings, SettingsOverrides, resolve};

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

pub type ProviderFactory =
    dyn Fn(&Server, String, Option<String>) -> Arc<dyn GitProvider> + Send + Sync;

const MAX_LOG_LINES: usize = 500;

pub struct Service {
    pub db: Arc<Db>,
    pub secrets: Arc<dyn SecretStore>,
    pub cache: Arc<RepoCache>,
    pub sink: Arc<dyn UiSink>,
    pub queue: ReviewQueue,
    provider_factory: Box<ProviderFactory>,
    logs: Mutex<HashMap<String, Vec<ProgressEvent>>>,
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
        let git = env::resolve_tool("git", config.git_path.as_deref())
            .unwrap_or_else(|_| PathBuf::from("git"));
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
        }))
    }

    pub fn provider(&self, server: &Server) -> AppResult<Arc<dyn GitProvider>> {
        let token = self.secrets.require(&server.id)?;
        Ok((self.provider_factory)(
            server,
            token,
            server.username.clone(),
        ))
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
        let new_token = input
            .token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty());
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
        let username = (self.provider_factory)(&probe, token.clone(), None)
            .current_user()
            .await?;

        let server = match &existing {
            Some(old) => {
                // Token before row: a failed keychain write must not leave the new URL paired
                // with the old token.
                let previous_token = match new_token {
                    Some(t) => {
                        let previous = self.secrets.get(&old.id)?;
                        self.secrets.set(&old.id, t)?;
                        previous
                    }
                    None => None,
                };
                match self.db.update_server(&old.id, input) {
                    Ok(server) => server,
                    Err(e) => {
                        if let Some(previous) = previous_token {
                            let _ = self.secrets.set(&old.id, &previous);
                        }
                        return Err(e);
                    }
                }
            }
            None => {
                let server = self.db.insert_server(input)?;
                if let Err(e) = self.secrets.set(&server.id, &token) {
                    // Never keep a server whose token could not be stored securely.
                    let _ = self.db.delete_server(&server.id);
                    return Err(e);
                }
                server
            }
        };
        self.db.set_server_username(&server.id, &username)?;
        self.db.get_server(&server.id)
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
            let mut logs = self.logs.lock().unwrap_or_else(|e| e.into_inner());
            let log = logs.entry(review_id.to_string()).or_default();
            if log.len() >= MAX_LOG_LINES {
                log.remove(0);
            }
            log.push(event.clone());
        }
        self.sink.review_progress(&event);
    }

    pub fn progress_log(&self, review_id: &str) -> Vec<ProgressEvent> {
        self.logs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(review_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn publish_review(&self, review_id: &str) {
        if let Ok(review) = self.db.get_review(review_id) {
            self.sink.review_updated(&review);
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
