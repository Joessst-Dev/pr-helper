use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::settings::SettingsOverrides;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ProviderKind {
    Github,
    Gitlab,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "github" => Some(Self::Github),
            "gitlab" => Some(Self::Gitlab),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Server {
    pub id: String,
    pub kind: ProviderKind,
    pub name: String,
    /// API base URL, e.g. `https://api.github.com`, `https://ghe.corp/api/v3`, `https://gitlab.corp`.
    pub base_url: String,
    pub username: Option<String>,
    /// Extra scope qualifiers for listing PRs. GitHub: search qualifiers (`org:acme repo:a/b`).
    /// GitLab: comma separated group paths. Empty means "everything visible to me".
    pub pr_scope: Option<String>,
    pub settings: SettingsOverrides,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ServerInput {
    pub kind: ProviderKind,
    pub name: String,
    pub base_url: String,
    pub pr_scope: Option<String>,
    /// Only set when the token should be (re)stored.
    pub token: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Nit,
}

impl Severity {
    /// Lower rank sorts first.
    pub fn rank(self) -> u8 {
        match self {
            Self::Critical => 0,
            Self::High => 1,
            Self::Medium => 2,
            Self::Low => 3,
            Self::Nit => 4,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Nit => "nit",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "critical" | "blocker" => Some(Self::Critical),
            "high" | "major" => Some(Self::High),
            "medium" | "moderate" => Some(Self::Medium),
            "low" | "minor" => Some(Self::Low),
            "nit" | "nitpick" | "trivial" => Some(Self::Nit),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Verdict {
    Approve,
    RequestChanges,
    Comment,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::RequestChanges => "request_changes",
            Self::Comment => "comment",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "approve" => Some(Self::Approve),
            "request_changes" => Some(Self::RequestChanges),
            "comment" => Some(Self::Comment),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PullRequest {
    pub server_id: String,
    /// Full repo path: `owner/name` (GitHub) or `group/sub/project` (GitLab).
    pub repo: String,
    /// GitHub: `owner/name`; GitLab: numeric project id.
    pub project_id: String,
    #[ts(type = "number")]
    pub number: u64,
    pub title: String,
    pub description: String,
    pub author: String,
    pub web_url: String,
    pub clone_url: Option<String>,
    pub head_sha: String,
    /// Merge base for GitLab (`diff_refs.base_sha`), base branch tip for GitHub.
    pub base_sha: Option<String>,
    /// GitLab `diff_refs.start_sha`, needed for inline positions.
    pub start_sha: Option<String>,
    pub head_ref: String,
    pub base_ref: String,
    pub draft: bool,
    pub state: PrState,
    pub updated_at: String,
    pub review_requested: bool,
    pub authored_by_me: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TrackedPr {
    pub server_id: String,
    pub repo: String,
    pub project_id: String,
    #[ts(type = "number")]
    pub number: u64,
    pub title: String,
    pub author: String,
    pub web_url: String,
    pub last_seen_head_sha: Option<String>,
    pub last_reviewed_sha: Option<String>,
    pub last_verdict: Option<Verdict>,
    pub has_new_changes: bool,
    pub active: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ReviewStatus {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl ReviewStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "done" => Self::Done,
            "cancelled" => Self::Cancelled,
            _ => Self::Failed,
        }
    }

    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ReviewTrigger {
    Manual,
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Review {
    pub id: String,
    pub server_id: String,
    pub repo: String,
    pub project_id: String,
    #[ts(type = "number")]
    pub number: u64,
    pub pr_title: String,
    pub head_sha: String,
    pub base_sha: Option<String>,
    pub status: ReviewStatus,
    pub trigger: ReviewTrigger,
    pub session_id: Option<String>,
    pub verdict: Option<Verdict>,
    pub summary: Option<String>,
    pub cost_usd: Option<f64>,
    pub error: Option<String>,
    /// Set when the worktree was kept so the session can be continued in a terminal.
    pub worktree_path: Option<String>,
    pub approval_posted: bool,
    pub created_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Side {
    Right,
    Left,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Finding {
    pub id: String,
    pub review_id: String,
    pub position: u32,
    pub severity: Severity,
    pub category: String,
    pub title: String,
    pub file: Option<String>,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub side: Side,
    pub explanation: String,
    pub proposed_comment: String,
    pub suggestion: Option<String>,
    /// Whether the anchor is a commentable diff line; otherwise it is posted as a general comment.
    pub in_diff: bool,
    pub diff_excerpt: Option<String>,
    pub selected: bool,
    pub posted_at: Option<String>,
    pub posted_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Skill {
    pub id: String,
    pub server_id: String,
    /// `None` = applies to every repo on the server.
    pub repo: Option<String>,
    pub name: String,
    pub description: String,
    /// Directory containing a SKILL.md. Mutually exclusive with `content`.
    pub path: Option<String>,
    /// Inline SKILL.md body authored in the app.
    pub content: Option<String>,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SkillInput {
    pub server_id: String,
    pub repo: Option<String>,
    pub name: String,
    pub description: String,
    pub path: Option<String>,
    pub content: Option<String>,
    pub enabled: bool,
}
