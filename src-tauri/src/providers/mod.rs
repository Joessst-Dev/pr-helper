pub mod github;
pub mod gitlab;

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::db::models::{ProviderKind, PullRequest, Server, Side};
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PrFilter {
    /// Everything visible to the user within the server's scope.
    All,
    ReviewRequested,
    Authored,
    Involved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewEvent {
    Comment,
    RequestChanges,
    Approve,
}

#[derive(Debug, Clone)]
pub struct InlineComment {
    pub finding_id: String,
    pub path: String,
    pub old_path: String,
    pub line: u32,
    /// First line of a multi-line range on the same side, if any.
    pub start_line: Option<u32>,
    pub side: Side,
    pub new_line: Option<u32>,
    pub old_line: Option<u32>,
    pub body: String,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReviewSubmission {
    pub event: ReviewEvent,
    pub body: String,
    pub inline: Vec<InlineComment>,
}

#[derive(Debug, Clone, Default)]
pub struct PostResult {
    /// finding id -> provider reference (comment URL or id).
    pub posted: Vec<(String, Option<String>)>,
    /// Inline comments the provider rejected; they were folded into the review body instead.
    pub folded: Vec<String>,
    pub review_url: Option<String>,
}

#[async_trait]
pub trait GitProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    async fn current_user(&self) -> AppResult<String>;
    async fn list_open_prs(&self, filter: PrFilter) -> AppResult<Vec<PullRequest>>;
    async fn get_pr(&self, project_id: &str, number: u64) -> AppResult<PullRequest>;
    async fn post_review(
        &self,
        pr: &PullRequest,
        submission: &ReviewSubmission,
    ) -> AppResult<PostResult>;
    async fn approve(&self, pr: &PullRequest, body: &str) -> AppResult<()>;
    /// `Authorization` header for git over HTTPS.
    fn git_auth_header(&self) -> String;
    fn pr_fetch_ref(&self, number: u64) -> String;
}

pub fn basic_auth_header(user: &str, token: &str) -> String {
    let creds = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{token}"));
    format!("Authorization: Basic {creds}")
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("pr-helper/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .expect("reqwest client")
}

pub fn make_provider(
    server: &Server,
    token: String,
    username: Option<String>,
) -> Arc<dyn GitProvider> {
    let client = http_client();
    match server.kind {
        ProviderKind::Github => Arc::new(github::GitHub::new(server, token, username, client)),
        ProviderKind::Gitlab => Arc::new(gitlab::GitLab::new(server, token, username, client)),
    }
}

/// Turn a non-2xx response into an error with the provider's message.
pub async fn check(
    provider: &'static str,
    resp: reqwest::Response,
) -> AppResult<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            let m = v.get("message").or_else(|| v.get("error")).cloned()?;
            let mut text = match m {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            };
            if let Some(errors) = v.get("errors") {
                text.push_str(&format!(" {errors}"));
            }
            Some(text)
        })
        .unwrap_or_else(|| body.chars().take(500).collect());
    Err(AppError::Api {
        provider,
        status: status.as_u16(),
        message,
    })
}

/// Markdown block appended under a heading when a comment can't be attached inline.
pub fn fold_comment(c: &InlineComment, suggestion_fence: &str) -> String {
    let mut s = format!("**{}:{}**\n\n{}", c.path, c.line, c.body);
    if let Some(sugg) = &c.suggestion {
        s.push_str(&format!("\n\n```{suggestion_fence}\n{sugg}\n```"));
    }
    s
}
