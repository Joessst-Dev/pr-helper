use rusqlite::Connection;

/// Append-only. Each entry bumps `PRAGMA user_version` by one.
const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE app_kv (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE servers (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL,
    name       TEXT NOT NULL,
    base_url   TEXT NOT NULL,
    username   TEXT,
    pr_scope   TEXT,
    settings   TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);

CREATE TABLE repo_settings (
    server_id TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    repo      TEXT NOT NULL,
    settings  TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (server_id, repo)
);

CREATE TABLE skills (
    id          TEXT PRIMARY KEY,
    server_id   TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    repo        TEXT,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    path        TEXT,
    content     TEXT,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL
);

CREATE TABLE tracked_prs (
    server_id          TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    repo               TEXT NOT NULL,
    number             INTEGER NOT NULL,
    project_id         TEXT NOT NULL,
    title              TEXT NOT NULL,
    author             TEXT NOT NULL,
    web_url            TEXT NOT NULL,
    last_seen_head_sha TEXT,
    last_reviewed_sha  TEXT,
    last_verdict       TEXT,
    has_new_changes    INTEGER NOT NULL DEFAULT 0,
    active             INTEGER NOT NULL DEFAULT 1,
    updated_at         TEXT NOT NULL,
    PRIMARY KEY (server_id, repo, number)
);

CREATE TABLE reviews (
    id              TEXT PRIMARY KEY,
    server_id       TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    repo            TEXT NOT NULL,
    project_id      TEXT NOT NULL,
    number          INTEGER NOT NULL,
    pr_title        TEXT NOT NULL,
    head_sha        TEXT NOT NULL,
    base_sha        TEXT,
    status          TEXT NOT NULL,
    trigger         TEXT NOT NULL,
    session_id      TEXT,
    verdict         TEXT,
    summary         TEXT,
    cost_usd        REAL,
    error           TEXT,
    worktree_path   TEXT,
    approval_posted INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL,
    finished_at     TEXT
);
CREATE INDEX reviews_pr ON reviews (server_id, repo, number, created_at);

CREATE TABLE findings (
    id               TEXT PRIMARY KEY,
    review_id        TEXT NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    position         INTEGER NOT NULL,
    severity         TEXT NOT NULL,
    category         TEXT NOT NULL,
    title            TEXT NOT NULL,
    file             TEXT,
    line_start       INTEGER,
    line_end         INTEGER,
    side             TEXT NOT NULL,
    explanation      TEXT NOT NULL,
    proposed_comment TEXT NOT NULL,
    suggestion       TEXT,
    in_diff          INTEGER NOT NULL,
    diff_excerpt     TEXT,
    selected         INTEGER NOT NULL,
    posted_at        TEXT,
    posted_ref       TEXT
);
CREATE INDEX findings_review ON findings (review_id, position);
"#,
    r#"
CREATE TABLE ticket_sources (
    id           TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    name         TEXT NOT NULL,
    base_url     TEXT NOT NULL DEFAULT '',
    email        TEXT,
    mcp_server   TEXT,
    mcp_tools    TEXT,
    model        TEXT,
    extra_fields TEXT,
    username     TEXT,
    created_at   TEXT NOT NULL
);

ALTER TABLE reviews ADD COLUMN tickets TEXT;
"#,
    r#"
CREATE TABLE review_files (
    review_id      TEXT NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    position       INTEGER NOT NULL,
    path           TEXT NOT NULL,
    reason         TEXT NOT NULL,
    focus_lines    TEXT NOT NULL,
    is_binary      INTEGER NOT NULL,
    truncated      INTEGER NOT NULL,
    content        TEXT,
    diff           TEXT NOT NULL,
    diff_truncated INTEGER NOT NULL,
    PRIMARY KEY (review_id, path)
);
"#,
];

pub fn run(conn: &mut Connection) -> rusqlite::Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}
