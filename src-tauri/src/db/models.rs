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

/// From most to least severe.
// Sorting relies on the derived `Ord` following this declaration order.
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
    /// People other than the author who were asked to review or already reviewed, one entry
    /// per person. Includes the user (`is_me`).
    #[serde(default)]
    pub reviewers: Vec<PrReviewer>,
    /// Approved by enough reviewers, as far as the forge tells.
    #[serde(default)]
    pub approved: bool,
}

/// Where a reviewer stands on a PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReviewerState {
    Requested,
    Commented,
    ChangesRequested,
    Approved,
}

impl ReviewerState {
    /// Which state wins when one person shows up more than once: a verdict beats a comment,
    /// which beats a pending request.
    pub fn rank(self) -> u8 {
        match self {
            Self::Requested => 0,
            Self::Commented => 1,
            Self::ChangesRequested => 2,
            Self::Approved => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PrReviewer {
    pub login: String,
    pub state: ReviewerState,
    pub is_me: bool,
}

/// One comment someone left on a PR. Author-controlled text: display only.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PrComment {
    pub id: String,
    pub author: String,
    pub is_me: bool,
    pub body: String,
    pub created_at: String,
    pub url: Option<String>,
    /// Set on the summary of a submitted review.
    pub review_state: Option<ReviewerState>,
}

/// A conversation on a PR: a general comment, a review summary or an inline thread.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PrThread {
    pub id: String,
    /// Set for inline threads.
    pub file: Option<String>,
    #[ts(type = "number | null")]
    pub line: Option<u64>,
    /// `None` when the thread can't be resolved.
    pub resolved: Option<bool>,
    /// The code it points at has changed since (GitHub only).
    pub outdated: bool,
    pub comments: Vec<PrComment>,
}

#[derive(Debug, Clone, Default, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PrComments {
    /// Oldest first.
    pub threads: Vec<PrThread>,
    /// The forge has more than was loaded.
    pub truncated: bool,
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

    /// Unknown values read as `Failed`: a review the app can't account for did not finish.
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

impl ReviewTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "manual" => Some(Self::Manual),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
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
    /// Tickets that were loaded as context for this review.
    pub tickets: Vec<TicketRef>,
}

/// A ticket that was given to the reviewer, for linking in the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TicketRef {
    pub key: String,
    pub url: Option<String>,
}

/// A changed file the reviewer recommends reading by hand, with its reason. The code itself is
/// loaded separately (`CriticalFileContent`) because it can be large.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CriticalFile {
    pub path: String,
    /// Written by the reviewer for the user. Never posted.
    pub reason: String,
    /// New-file line ranges the reviewer points at.
    pub focus_lines: Vec<LineRange>,
    /// Why the file's code is not available, if it isn't. The diff may still be.
    pub note: Option<FileNote>,
}

/// Why a critical file has no stored code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum FileNote {
    /// Binary at the reviewed commit.
    Binary,
    /// Too large to store.
    TooLarge,
    /// Deleted by the PR.
    Deleted,
    /// Not a regular file at the reviewed commit, e.g. a submodule.
    NotAFile,
    /// Reading it from git failed; the review log has the error.
    Unreadable,
    /// Only the latest review of a PR keeps the code; this is an older review.
    Pruned,
}

impl FileNote {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::TooLarge => "too_large",
            Self::Deleted => "deleted",
            Self::NotAFile => "not_a_file",
            Self::Unreadable => "unreadable",
            Self::Pruned => "pruned",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "binary" => Some(Self::Binary),
            "too_large" => Some(Self::TooLarge),
            "deleted" => Some(Self::Deleted),
            "not_a_file" => Some(Self::NotAFile),
            "unreadable" => Some(Self::Unreadable),
            "pruned" => Some(Self::Pruned),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

/// The snapshot of a critical file taken when the review finished.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CriticalFileContent {
    /// The file at the reviewed head commit. `None` when `CriticalFile::note` says why.
    pub content: Option<String>,
    /// The file's diff lines (merge base to head).
    pub diff: Vec<FileDiffLine>,
    /// The diff was cut off because it was too long.
    pub diff_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileDiffLine {
    pub kind: DiffLineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

/// What gets stored per critical file.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewFile {
    pub meta: CriticalFile,
    pub content: CriticalFileContent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Side {
    Right,
    Left,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Left => "left",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "right" => Some(Self::Right),
            "left" => Some(Self::Left),
            _ => None,
        }
    }
}

/// Who wrote a finding: the reviewer, or the user on a file they reviewed by hand.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum FindingSource {
    #[default]
    Reviewer,
    User,
}

impl FindingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reviewer => "reviewer",
            Self::User => "user",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "reviewer" => Some(Self::Reviewer),
            "user" => Some(Self::User),
            _ => None,
        }
    }
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
    pub source: FindingSource,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TicketSourceKind {
    /// Atlassian Cloud: email plus API token (Basic auth).
    JiraCloud,
    /// Jira Server / Data Center: personal access token (Bearer).
    JiraServer,
    /// An MCP server configured in the user's Claude Code. The app stores no credentials.
    Mcp,
}

impl TicketSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::JiraCloud => "jira_cloud",
            Self::JiraServer => "jira_server",
            Self::Mcp => "mcp",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "jira_cloud" => Some(Self::JiraCloud),
            "jira_server" => Some(Self::JiraServer),
            "mcp" => Some(Self::Mcp),
            _ => None,
        }
    }

    pub fn needs_token(self) -> bool {
        self != Self::Mcp
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TicketSource {
    pub id: String,
    pub kind: TicketSourceKind,
    pub name: String,
    /// Jira site URL, e.g. `https://acme.atlassian.net`. Optional for MCP (only used for links).
    pub base_url: String,
    /// Jira Cloud account email (Basic auth user).
    pub email: Option<String>,
    /// MCP server name as configured in Claude Code, e.g. `atlassian`.
    pub mcp_server: Option<String>,
    /// Comma separated MCP tool names the fetcher may call.
    pub mcp_tools: Option<String>,
    /// Model for the MCP fetcher run. Empty = haiku.
    pub model: Option<String>,
    /// Comma separated extra field ids, e.g. `customfield_10035` (acceptance criteria).
    pub extra_fields: Option<String>,
    /// Jira account the token belongs to, filled in on save.
    pub username: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TicketSourceInput {
    pub kind: TicketSourceKind,
    pub name: String,
    pub base_url: String,
    pub email: Option<String>,
    pub mcp_server: Option<String>,
    pub mcp_tools: Option<String>,
    pub model: Option<String>,
    pub extra_fields: Option<String>,
    /// Only set when the token should be (re)stored.
    pub token: Option<String>,
}
