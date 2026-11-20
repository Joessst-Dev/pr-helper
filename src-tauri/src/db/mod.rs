pub mod migrations;
pub mod models;

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::{AppError, AppResult};
use crate::settings::{AppConfig, SettingsOverrides};
use models::*;

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub struct Db {
    conn: Mutex<Connection>,
}

const KV_GLOBAL_SETTINGS: &str = "global_settings";
const KV_APP_CONFIG: &str = "app_config";

impl Db {
    pub fn open(path: &Path) -> AppResult<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> AppResult<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> AppResult<Self> {
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        migrations::run(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> AppResult<T> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(f(&conn)?)
    }

    // ---- key/value -------------------------------------------------------

    fn kv_get<T: serde::de::DeserializeOwned + Default>(&self, key: &str) -> AppResult<T> {
        let raw: Option<String> = self.with(|c| {
            c.query_row("SELECT value FROM app_kv WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()
        })?;
        Ok(match raw {
            Some(s) => serde_json::from_str(&s)?,
            None => T::default(),
        })
    }

    fn kv_set<T: serde::Serialize>(&self, key: &str, value: &T) -> AppResult<()> {
        let raw = serde_json::to_string(value)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO app_kv (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, raw],
            )
        })?;
        Ok(())
    }

    pub fn global_settings(&self) -> AppResult<SettingsOverrides> {
        self.kv_get(KV_GLOBAL_SETTINGS)
    }

    pub fn set_global_settings(&self, s: &SettingsOverrides) -> AppResult<()> {
        self.kv_set(KV_GLOBAL_SETTINGS, s)
    }

    pub fn app_config(&self) -> AppResult<AppConfig> {
        self.kv_get(KV_APP_CONFIG)
    }

    pub fn set_app_config(&self, c: &AppConfig) -> AppResult<()> {
        self.kv_set(KV_APP_CONFIG, c)
    }

    // ---- servers ---------------------------------------------------------

    fn server_from_row(r: &Row) -> rusqlite::Result<Server> {
        let kind: String = r.get("kind")?;
        let settings: String = r.get("settings")?;
        Ok(Server {
            id: r.get("id")?,
            kind: ProviderKind::parse(&kind).unwrap_or(ProviderKind::Github),
            name: r.get("name")?,
            base_url: r.get("base_url")?,
            username: r.get("username")?,
            pr_scope: r.get("pr_scope")?,
            settings: serde_json::from_str(&settings).unwrap_or_default(),
            created_at: r.get("created_at")?,
        })
    }

    pub fn list_servers(&self) -> AppResult<Vec<Server>> {
        self.with(|c| {
            let mut stmt = c.prepare("SELECT * FROM servers ORDER BY name COLLATE NOCASE")?;
            let rows = stmt.query_map([], Self::server_from_row)?;
            rows.collect()
        })
    }

    pub fn get_server(&self, id: &str) -> AppResult<Server> {
        self.with(|c| {
            c.query_row(
                "SELECT * FROM servers WHERE id = ?1",
                [id],
                Self::server_from_row,
            )
            .optional()
        })?
        .ok_or_else(|| AppError::NotFound(format!("server {id}")))
    }

    pub fn insert_server(&self, input: &ServerInput) -> AppResult<Server> {
        let id = new_id();
        self.with(|c| {
            c.execute(
                "INSERT INTO servers (id, kind, name, base_url, pr_scope, settings, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, '{}', ?6)",
                params![
                    id,
                    input.kind.as_str(),
                    input.name.trim(),
                    normalize_base_url(&input.base_url),
                    input
                        .pr_scope
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty()),
                    now()
                ],
            )
        })?;
        self.get_server(&id)
    }

    pub fn update_server(&self, id: &str, input: &ServerInput) -> AppResult<Server> {
        self.with(|c| {
            c.execute(
                "UPDATE servers SET kind = ?2, name = ?3, base_url = ?4, pr_scope = ?5 WHERE id = ?1",
                params![
                    id,
                    input.kind.as_str(),
                    input.name.trim(),
                    normalize_base_url(&input.base_url),
                    input.pr_scope.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                ],
            )
        })?;
        self.get_server(id)
    }

    pub fn set_server_username(&self, id: &str, username: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE servers SET username = ?2 WHERE id = ?1",
                params![id, username],
            )
        })?;
        Ok(())
    }

    pub fn set_server_settings(&self, id: &str, s: &SettingsOverrides) -> AppResult<()> {
        let raw = serde_json::to_string(s)?;
        self.with(|c| {
            c.execute(
                "UPDATE servers SET settings = ?2 WHERE id = ?1",
                params![id, raw],
            )
        })?;
        Ok(())
    }

    pub fn delete_server(&self, id: &str) -> AppResult<()> {
        self.with(|c| c.execute("DELETE FROM servers WHERE id = ?1", [id]))?;
        Ok(())
    }

    // ---- repo settings ---------------------------------------------------

    pub fn repo_settings(
        &self,
        server_id: &str,
        repo: &str,
    ) -> AppResult<Option<SettingsOverrides>> {
        let raw: Option<String> = self.with(|c| {
            c.query_row(
                "SELECT settings FROM repo_settings WHERE server_id = ?1 AND repo = ?2",
                params![server_id, repo],
                |r| r.get(0),
            )
            .optional()
        })?;
        Ok(raw.map(|s| serde_json::from_str(&s).unwrap_or_default()))
    }

    pub fn set_repo_settings(
        &self,
        server_id: &str,
        repo: &str,
        s: &SettingsOverrides,
    ) -> AppResult<()> {
        if *s == SettingsOverrides::default() {
            self.with(|c| {
                c.execute(
                    "DELETE FROM repo_settings WHERE server_id = ?1 AND repo = ?2",
                    params![server_id, repo],
                )
            })?;
            return Ok(());
        }
        let raw = serde_json::to_string(s)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO repo_settings (server_id, repo, settings) VALUES (?1, ?2, ?3)
                 ON CONFLICT(server_id, repo) DO UPDATE SET settings = excluded.settings",
                params![server_id, repo, raw],
            )
        })?;
        Ok(())
    }

    pub fn list_repo_settings(
        &self,
        server_id: &str,
    ) -> AppResult<Vec<(String, SettingsOverrides)>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT repo, settings FROM repo_settings WHERE server_id = ?1 ORDER BY repo",
            )?;
            let rows = stmt.query_map([server_id], |r| {
                let raw: String = r.get(1)?;
                Ok((r.get(0)?, serde_json::from_str(&raw).unwrap_or_default()))
            })?;
            rows.collect()
        })
    }

    // ---- skills ----------------------------------------------------------

    fn skill_from_row(r: &Row) -> rusqlite::Result<Skill> {
        Ok(Skill {
            id: r.get("id")?,
            server_id: r.get("server_id")?,
            repo: r.get("repo")?,
            name: r.get("name")?,
            description: r.get("description")?,
            path: r.get("path")?,
            content: r.get("content")?,
            enabled: r.get("enabled")?,
            created_at: r.get("created_at")?,
        })
    }

    pub fn list_skills(&self, server_id: &str) -> AppResult<Vec<Skill>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT * FROM skills WHERE server_id = ?1 ORDER BY repo IS NOT NULL, repo, name",
            )?;
            let rows = stmt.query_map([server_id], Self::skill_from_row)?;
            rows.collect()
        })
    }

    /// Enabled server-wide skills plus enabled skills scoped to `repo`.
    pub fn skills_for_review(&self, server_id: &str, repo: &str) -> AppResult<Vec<Skill>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT * FROM skills WHERE server_id = ?1 AND enabled = 1
                 AND (repo IS NULL OR repo = ?2) ORDER BY name",
            )?;
            let rows = stmt.query_map(params![server_id, repo], Self::skill_from_row)?;
            rows.collect()
        })
    }

    pub fn upsert_skill(&self, id: Option<&str>, input: &SkillInput) -> AppResult<Skill> {
        let id = id.map(str::to_string).unwrap_or_else(new_id);
        let repo = input
            .repo
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        self.with(|c| {
            c.execute(
                "INSERT INTO skills (id, server_id, repo, name, description, path, content, enabled, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET repo = excluded.repo, name = excluded.name,
                   description = excluded.description, path = excluded.path,
                   content = excluded.content, enabled = excluded.enabled",
                params![
                    id,
                    input.server_id,
                    repo,
                    input.name.trim(),
                    input.description.trim(),
                    input.path,
                    input.content,
                    input.enabled,
                    now()
                ],
            )
        })?;
        self.with(|c| {
            c.query_row(
                "SELECT * FROM skills WHERE id = ?1",
                [&id],
                Self::skill_from_row,
            )
        })
    }

    pub fn delete_skill(&self, id: &str) -> AppResult<()> {
        self.with(|c| c.execute("DELETE FROM skills WHERE id = ?1", [id]))?;
        Ok(())
    }

    // ---- ticket sources --------------------------------------------------

    fn ticket_source_from_row(r: &Row) -> rusqlite::Result<TicketSource> {
        let kind: String = r.get("kind")?;
        Ok(TicketSource {
            id: r.get("id")?,
            kind: TicketSourceKind::parse(&kind).unwrap_or(TicketSourceKind::JiraCloud),
            name: r.get("name")?,
            base_url: r.get("base_url")?,
            email: r.get("email")?,
            mcp_server: r.get("mcp_server")?,
            mcp_tools: r.get("mcp_tools")?,
            model: r.get("model")?,
            extra_fields: r.get("extra_fields")?,
            username: r.get("username")?,
            created_at: r.get("created_at")?,
        })
    }

    pub fn list_ticket_sources(&self) -> AppResult<Vec<TicketSource>> {
        self.with(|c| {
            let mut stmt =
                c.prepare("SELECT * FROM ticket_sources ORDER BY name COLLATE NOCASE")?;
            let rows = stmt.query_map([], Self::ticket_source_from_row)?;
            rows.collect()
        })
    }

    pub fn get_ticket_source(&self, id: &str) -> AppResult<TicketSource> {
        self.with(|c| {
            c.query_row(
                "SELECT * FROM ticket_sources WHERE id = ?1",
                [id],
                Self::ticket_source_from_row,
            )
            .optional()
        })?
        .ok_or_else(|| AppError::NotFound(format!("ticket source {id}")))
    }

    /// Inserts (`id` = None) or updates a ticket source. Never stores a token.
    pub fn upsert_ticket_source(
        &self,
        id: Option<&str>,
        input: &TicketSourceInput,
        username: Option<&str>,
    ) -> AppResult<TicketSource> {
        let id = id.map(str::to_string).unwrap_or_else(new_id);
        let opt = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        self.with(|c| {
            c.execute(
                "INSERT INTO ticket_sources (id, kind, name, base_url, email, mcp_server, mcp_tools,
                   model, extra_fields, username, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(id) DO UPDATE SET kind = excluded.kind, name = excluded.name,
                   base_url = excluded.base_url, email = excluded.email,
                   mcp_server = excluded.mcp_server, mcp_tools = excluded.mcp_tools,
                   model = excluded.model, extra_fields = excluded.extra_fields,
                   username = excluded.username",
                params![
                    id,
                    input.kind.as_str(),
                    input.name.trim(),
                    normalize_base_url(&input.base_url),
                    opt(&input.email),
                    opt(&input.mcp_server),
                    opt(&input.mcp_tools),
                    opt(&input.model),
                    opt(&input.extra_fields),
                    username,
                    now()
                ],
            )
        })?;
        self.get_ticket_source(&id)
    }

    pub fn delete_ticket_source(&self, id: &str) -> AppResult<()> {
        self.with(|c| c.execute("DELETE FROM ticket_sources WHERE id = ?1", [id]))?;
        Ok(())
    }

    // ---- tracked PRs -----------------------------------------------------

    fn tracked_from_row(r: &Row) -> rusqlite::Result<TrackedPr> {
        let verdict: Option<String> = r.get("last_verdict")?;
        Ok(TrackedPr {
            server_id: r.get("server_id")?,
            repo: r.get("repo")?,
            project_id: r.get("project_id")?,
            number: r.get::<_, i64>("number")? as u64,
            title: r.get("title")?,
            author: r.get("author")?,
            web_url: r.get("web_url")?,
            last_seen_head_sha: r.get("last_seen_head_sha")?,
            last_reviewed_sha: r.get("last_reviewed_sha")?,
            last_verdict: verdict.as_deref().and_then(Verdict::parse),
            has_new_changes: r.get("has_new_changes")?,
            active: r.get("active")?,
            updated_at: r.get("updated_at")?,
        })
    }

    /// Start tracking a PR (or refresh its metadata) when it gets reviewed.
    pub fn track_pr(&self, pr: &PullRequest) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO tracked_prs (server_id, repo, number, project_id, title, author, web_url,
                   last_seen_head_sha, has_new_changes, active, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 1, ?9)
                 ON CONFLICT(server_id, repo, number) DO UPDATE SET project_id = excluded.project_id,
                   title = excluded.title, author = excluded.author, web_url = excluded.web_url,
                   last_seen_head_sha = excluded.last_seen_head_sha, active = 1,
                   updated_at = excluded.updated_at",
                params![
                    pr.server_id,
                    pr.repo,
                    pr.number as i64,
                    pr.project_id,
                    pr.title,
                    pr.author,
                    pr.web_url,
                    pr.head_sha,
                    now()
                ],
            )
        })?;
        Ok(())
    }

    pub fn get_tracked(
        &self,
        server_id: &str,
        repo: &str,
        number: u64,
    ) -> AppResult<Option<TrackedPr>> {
        self.with(|c| {
            c.query_row(
                "SELECT * FROM tracked_prs WHERE server_id = ?1 AND repo = ?2 AND number = ?3",
                params![server_id, repo, number as i64],
                Self::tracked_from_row,
            )
            .optional()
        })
    }

    pub fn list_tracked(&self, only_active: bool) -> AppResult<Vec<TrackedPr>> {
        self.with(|c| {
            let sql = if only_active {
                "SELECT * FROM tracked_prs WHERE active = 1 ORDER BY updated_at DESC"
            } else {
                "SELECT * FROM tracked_prs ORDER BY updated_at DESC"
            };
            let mut stmt = c.prepare(sql)?;
            let rows = stmt.query_map([], Self::tracked_from_row)?;
            rows.collect()
        })
    }

    pub fn set_tracked_head(
        &self,
        server_id: &str,
        repo: &str,
        number: u64,
        head_sha: &str,
        has_new_changes: bool,
    ) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE tracked_prs SET last_seen_head_sha = ?4, has_new_changes = ?5, updated_at = ?6
                 WHERE server_id = ?1 AND repo = ?2 AND number = ?3",
                params![server_id, repo, number as i64, head_sha, has_new_changes, now()],
            )
        })?;
        Ok(())
    }

    pub fn set_tracked_reviewed(
        &self,
        server_id: &str,
        repo: &str,
        number: u64,
        reviewed_sha: &str,
        verdict: Option<Verdict>,
    ) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE tracked_prs SET last_reviewed_sha = ?4, last_verdict = ?5,
                   has_new_changes = (last_seen_head_sha IS NOT NULL AND last_seen_head_sha != ?4),
                   updated_at = ?6
                 WHERE server_id = ?1 AND repo = ?2 AND number = ?3",
                params![
                    server_id,
                    repo,
                    number as i64,
                    reviewed_sha,
                    verdict.map(Verdict::as_str),
                    now()
                ],
            )
        })?;
        Ok(())
    }

    pub fn set_tracked_active(
        &self,
        server_id: &str,
        repo: &str,
        number: u64,
        active: bool,
    ) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE tracked_prs SET active = ?4, updated_at = ?5
                 WHERE server_id = ?1 AND repo = ?2 AND number = ?3",
                params![server_id, repo, number as i64, active, now()],
            )
        })?;
        Ok(())
    }

    pub fn untrack(&self, server_id: &str, repo: &str, number: u64) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "DELETE FROM tracked_prs WHERE server_id = ?1 AND repo = ?2 AND number = ?3",
                params![server_id, repo, number as i64],
            )
        })?;
        Ok(())
    }

    // ---- reviews ---------------------------------------------------------

    fn review_from_row(r: &Row) -> rusqlite::Result<Review> {
        let status: String = r.get("status")?;
        let trigger: String = r.get("trigger")?;
        let verdict: Option<String> = r.get("verdict")?;
        Ok(Review {
            id: r.get("id")?,
            server_id: r.get("server_id")?,
            repo: r.get("repo")?,
            project_id: r.get("project_id")?,
            number: r.get::<_, i64>("number")? as u64,
            pr_title: r.get("pr_title")?,
            head_sha: r.get("head_sha")?,
            base_sha: r.get("base_sha")?,
            status: ReviewStatus::parse(&status),
            trigger: if trigger == "auto" {
                ReviewTrigger::Auto
            } else {
                ReviewTrigger::Manual
            },
            session_id: r.get("session_id")?,
            verdict: verdict.as_deref().and_then(Verdict::parse),
            summary: r.get("summary")?,
            cost_usd: r.get("cost_usd")?,
            error: r.get("error")?,
            worktree_path: r.get("worktree_path")?,
            approval_posted: r.get("approval_posted")?,
            created_at: r.get("created_at")?,
            finished_at: r.get("finished_at")?,
            tickets: r
                .get::<_, Option<String>>("tickets")?
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default(),
        })
    }

    pub fn insert_review(&self, pr: &PullRequest, trigger: ReviewTrigger) -> AppResult<Review> {
        let id = new_id();
        self.with(|c| {
            c.execute(
                "INSERT INTO reviews (id, server_id, repo, project_id, number, pr_title, head_sha, base_sha,
                   status, trigger, approval_posted, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'queued', ?9, 0, ?10)",
                params![
                    id,
                    pr.server_id,
                    pr.repo,
                    pr.project_id,
                    pr.number as i64,
                    pr.title,
                    pr.head_sha,
                    pr.base_sha,
                    if trigger == ReviewTrigger::Auto { "auto" } else { "manual" },
                    now()
                ],
            )
        })?;
        self.get_review(&id)
    }

    pub fn get_review(&self, id: &str) -> AppResult<Review> {
        self.with(|c| {
            c.query_row(
                "SELECT * FROM reviews WHERE id = ?1",
                [id],
                Self::review_from_row,
            )
            .optional()
        })?
        .ok_or_else(|| AppError::NotFound(format!("review {id}")))
    }

    pub fn list_reviews_for_pr(
        &self,
        server_id: &str,
        repo: &str,
        number: u64,
    ) -> AppResult<Vec<Review>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT * FROM reviews WHERE server_id = ?1 AND repo = ?2 AND number = ?3
                 ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map(
                params![server_id, repo, number as i64],
                Self::review_from_row,
            )?;
            rows.collect()
        })
    }

    pub fn list_recent_reviews(&self, limit: u32) -> AppResult<Vec<Review>> {
        self.with(|c| {
            let mut stmt = c.prepare("SELECT * FROM reviews ORDER BY created_at DESC LIMIT ?1")?;
            let rows = stmt.query_map([limit], Self::review_from_row)?;
            rows.collect()
        })
    }

    /// The newest finished review for a PR, used for incremental re-reviews.
    pub fn last_done_review(
        &self,
        server_id: &str,
        repo: &str,
        number: u64,
    ) -> AppResult<Option<Review>> {
        self.with(|c| {
            c.query_row(
                "SELECT * FROM reviews WHERE server_id = ?1 AND repo = ?2 AND number = ?3
                 AND status = 'done' ORDER BY created_at DESC LIMIT 1",
                params![server_id, repo, number as i64],
                Self::review_from_row,
            )
            .optional()
        })
    }

    pub fn set_review_status(
        &self,
        id: &str,
        status: ReviewStatus,
        error: Option<&str>,
    ) -> AppResult<()> {
        let finished = status.is_finished().then(now);
        self.with(|c| {
            c.execute(
                "UPDATE reviews SET status = ?2, error = COALESCE(?3, error),
                   finished_at = COALESCE(?4, finished_at) WHERE id = ?1",
                params![id, status.as_str(), error, finished],
            )
        })?;
        Ok(())
    }

    pub fn set_review_session(&self, id: &str, session_id: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE reviews SET session_id = ?2 WHERE id = ?1",
                params![id, session_id],
            )
        })?;
        Ok(())
    }

    pub fn set_review_base(&self, id: &str, base_sha: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE reviews SET base_sha = ?2 WHERE id = ?1",
                params![id, base_sha],
            )
        })?;
        Ok(())
    }

    pub fn set_review_worktree(&self, id: &str, path: Option<&str>) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE reviews SET worktree_path = ?2 WHERE id = ?1",
                params![id, path],
            )
        })?;
        Ok(())
    }

    /// Stores the outcome and findings of a successful run in one transaction.
    pub fn complete_review(
        &self,
        id: &str,
        verdict: Verdict,
        summary: &str,
        cost_usd: Option<f64>,
        findings: &[Finding],
    ) -> AppResult<()> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE reviews SET status = 'done', verdict = ?2, summary = ?3, cost_usd = ?4,
               finished_at = ?5, error = NULL WHERE id = ?1",
            params![id, verdict.as_str(), summary, cost_usd, now()],
        )?;
        tx.execute("DELETE FROM findings WHERE review_id = ?1", [id])?;
        for f in findings {
            tx.execute(
                "INSERT INTO findings (id, review_id, position, severity, category, title, file,
                   line_start, line_end, side, explanation, proposed_comment, suggestion, in_diff,
                   diff_excerpt, selected)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    f.id,
                    id,
                    f.position,
                    f.severity.as_str(),
                    f.category,
                    f.title,
                    f.file,
                    f.line_start,
                    f.line_end,
                    if f.side == Side::Left {
                        "left"
                    } else {
                        "right"
                    },
                    f.explanation,
                    f.proposed_comment,
                    f.suggestion,
                    f.in_diff,
                    f.diff_excerpt,
                    f.selected
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_review_tickets(&self, id: &str, tickets: &[TicketRef]) -> AppResult<()> {
        let raw = (!tickets.is_empty())
            .then(|| serde_json::to_string(tickets))
            .transpose()?;
        self.with(|c| {
            c.execute(
                "UPDATE reviews SET tickets = ?2 WHERE id = ?1",
                params![id, raw],
            )
        })?;
        Ok(())
    }

    pub fn set_approval_posted(&self, id: &str) -> AppResult<()> {
        self.with(|c| c.execute("UPDATE reviews SET approval_posted = 1 WHERE id = ?1", [id]))?;
        Ok(())
    }

    /// Reviews interrupted by an app exit can never finish; mark them failed on startup.
    pub fn fail_interrupted_reviews(&self) -> AppResult<usize> {
        self.with(|c| {
            c.execute(
                "UPDATE reviews SET status = 'failed', error = 'Interrupted: the app was closed during the review',
                   finished_at = ?1 WHERE status IN ('queued', 'running')",
                [now()],
            )
        })
    }

    // ---- findings --------------------------------------------------------

    fn finding_from_row(r: &Row) -> rusqlite::Result<Finding> {
        let severity: String = r.get("severity")?;
        let side: String = r.get("side")?;
        Ok(Finding {
            id: r.get("id")?,
            review_id: r.get("review_id")?,
            position: r.get("position")?,
            severity: Severity::parse(&severity).unwrap_or(Severity::Low),
            category: r.get("category")?,
            title: r.get("title")?,
            file: r.get("file")?,
            line_start: r.get("line_start")?,
            line_end: r.get("line_end")?,
            side: if side == "left" {
                Side::Left
            } else {
                Side::Right
            },
            explanation: r.get("explanation")?,
            proposed_comment: r.get("proposed_comment")?,
            suggestion: r.get("suggestion")?,
            in_diff: r.get("in_diff")?,
            diff_excerpt: r.get("diff_excerpt")?,
            selected: r.get("selected")?,
            posted_at: r.get("posted_at")?,
            posted_ref: r.get("posted_ref")?,
        })
    }

    pub fn list_findings(&self, review_id: &str) -> AppResult<Vec<Finding>> {
        self.with(|c| {
            let mut stmt =
                c.prepare("SELECT * FROM findings WHERE review_id = ?1 ORDER BY position")?;
            let rows = stmt.query_map([review_id], Self::finding_from_row)?;
            rows.collect()
        })
    }

    pub fn get_finding(&self, id: &str) -> AppResult<Finding> {
        self.with(|c| {
            c.query_row(
                "SELECT * FROM findings WHERE id = ?1",
                [id],
                Self::finding_from_row,
            )
            .optional()
        })?
        .ok_or_else(|| AppError::NotFound(format!("finding {id}")))
    }

    pub fn update_finding(
        &self,
        id: &str,
        selected: bool,
        proposed_comment: &str,
    ) -> AppResult<Finding> {
        self.with(|c| {
            c.execute(
                "UPDATE findings SET selected = ?2, proposed_comment = ?3 WHERE id = ?1",
                params![id, selected, proposed_comment],
            )
        })?;
        self.get_finding(id)
    }

    pub fn mark_finding_posted(&self, id: &str, posted_ref: Option<&str>) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE findings SET posted_at = ?2, posted_ref = ?3 WHERE id = ?1",
                params![id, now(), posted_ref],
            )
        })?;
        Ok(())
    }
}

pub fn normalize_base_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_input() -> ServerInput {
        ServerInput {
            kind: ProviderKind::Gitlab,
            name: "Corp GitLab".into(),
            base_url: "https://gitlab.corp/ ".into(),
            pr_scope: Some("  ".into()),
            token: None,
        }
    }

    fn sample_pr(server_id: &str) -> PullRequest {
        PullRequest {
            server_id: server_id.into(),
            repo: "team/app".into(),
            project_id: "42".into(),
            number: 7,
            title: "Add feature".into(),
            description: String::new(),
            author: "alice".into(),
            web_url: "https://gitlab.corp/team/app/-/merge_requests/7".into(),
            clone_url: None,
            head_sha: "aaa".into(),
            base_sha: Some("bbb".into()),
            start_sha: None,
            head_ref: "feature".into(),
            base_ref: "main".into(),
            draft: false,
            state: PrState::Open,
            updated_at: now(),
            review_requested: true,
            authored_by_me: false,
        }
    }

    #[test]
    fn server_crud_normalizes_input() {
        let db = Db::open_in_memory().unwrap();
        let s = db.insert_server(&server_input()).unwrap();
        assert_eq!(s.base_url, "https://gitlab.corp");
        assert_eq!(s.pr_scope, None);
        db.set_server_settings(
            &s.id,
            &SettingsOverrides {
                skip_nits: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(db.get_server(&s.id).unwrap().settings.skip_nits, Some(true));
        db.delete_server(&s.id).unwrap();
        assert!(db.list_servers().unwrap().is_empty());
    }

    #[test]
    fn skills_for_review_merges_server_and_repo_scope() {
        let db = Db::open_in_memory().unwrap();
        let s = db.insert_server(&server_input()).unwrap();
        let mk = |repo: Option<&str>, name: &str, enabled: bool| SkillInput {
            server_id: s.id.clone(),
            repo: repo.map(Into::into),
            name: name.into(),
            description: String::new(),
            path: None,
            content: Some("# skill".into()),
            enabled,
        };
        db.upsert_skill(None, &mk(None, "server-wide", true))
            .unwrap();
        db.upsert_skill(None, &mk(Some("team/app"), "app-only", true))
            .unwrap();
        db.upsert_skill(None, &mk(Some("team/other"), "other", true))
            .unwrap();
        db.upsert_skill(None, &mk(None, "disabled", false)).unwrap();
        let names: Vec<_> = db
            .skills_for_review(&s.id, "team/app")
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["app-only", "server-wide"]);
    }

    #[test]
    fn review_lifecycle_updates_tracking() {
        let db = Db::open_in_memory().unwrap();
        let s = db.insert_server(&server_input()).unwrap();
        let pr = sample_pr(&s.id);
        db.track_pr(&pr).unwrap();
        let review = db.insert_review(&pr, ReviewTrigger::Manual).unwrap();
        assert_eq!(review.status, ReviewStatus::Queued);

        let finding = Finding {
            id: new_id(),
            review_id: review.id.clone(),
            position: 0,
            severity: Severity::High,
            category: "bug".into(),
            title: "t".into(),
            file: Some("a.rs".into()),
            line_start: Some(3),
            line_end: None,
            side: Side::Right,
            explanation: "e".into(),
            proposed_comment: "c".into(),
            suggestion: None,
            in_diff: true,
            diff_excerpt: None,
            selected: true,
            posted_at: None,
            posted_ref: None,
        };
        db.complete_review(
            &review.id,
            Verdict::RequestChanges,
            "sum",
            Some(0.5),
            &[finding],
        )
        .unwrap();
        db.set_tracked_reviewed(&s.id, "team/app", 7, "aaa", Some(Verdict::RequestChanges))
            .unwrap();
        let t = db.get_tracked(&s.id, "team/app", 7).unwrap().unwrap();
        assert!(!t.has_new_changes);
        assert_eq!(t.last_verdict, Some(Verdict::RequestChanges));

        // A new push arrives.
        db.set_tracked_head(&s.id, "team/app", 7, "ccc", true)
            .unwrap();
        assert!(
            db.get_tracked(&s.id, "team/app", 7)
                .unwrap()
                .unwrap()
                .has_new_changes
        );

        let done = db.get_review(&review.id).unwrap();
        assert_eq!(done.status, ReviewStatus::Done);
        assert_eq!(db.list_findings(&review.id).unwrap().len(), 1);
        let last = db.last_done_review(&s.id, "team/app", 7).unwrap().unwrap();
        assert_eq!(last.id, review.id);
        assert!(last.tickets.is_empty());

        let refs = vec![TicketRef {
            key: "ABC-1".into(),
            url: Some("https://jira/browse/ABC-1".into()),
        }];
        db.set_review_tickets(&review.id, &refs).unwrap();
        assert_eq!(db.get_review(&review.id).unwrap().tickets, refs);
    }

    #[test]
    fn ticket_source_crud() {
        let db = Db::open_in_memory().unwrap();
        let input = TicketSourceInput {
            kind: TicketSourceKind::JiraCloud,
            name: " Acme Jira ".into(),
            base_url: "https://acme.atlassian.net/".into(),
            email: Some("me@acme.test".into()),
            mcp_server: Some("  ".into()),
            mcp_tools: None,
            model: None,
            extra_fields: Some("customfield_10035".into()),
            token: Some("must-not-be-stored".into()),
        };
        let s = db.upsert_ticket_source(None, &input, Some("Me")).unwrap();
        assert_eq!(s.name, "Acme Jira");
        assert_eq!(s.base_url, "https://acme.atlassian.net");
        assert_eq!(s.mcp_server, None);
        assert_eq!(s.username.as_deref(), Some("Me"));
        let mut edited = input.clone();
        edited.name = "Renamed".into();
        db.upsert_ticket_source(Some(&s.id), &edited, None).unwrap();
        let all = db.list_ticket_sources().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Renamed");
        db.delete_ticket_source(&s.id).unwrap();
        assert!(db.list_ticket_sources().unwrap().is_empty());
    }

    #[test]
    fn interrupted_reviews_fail_on_startup() {
        let db = Db::open_in_memory().unwrap();
        let s = db.insert_server(&server_input()).unwrap();
        let r = db
            .insert_review(&sample_pr(&s.id), ReviewTrigger::Auto)
            .unwrap();
        db.set_review_status(&r.id, ReviewStatus::Running, None)
            .unwrap();
        assert_eq!(db.fail_interrupted_reviews().unwrap(), 1);
        let r = db.get_review(&r.id).unwrap();
        assert_eq!(r.status, ReviewStatus::Failed);
        assert!(r.finished_at.is_some());
    }
}
