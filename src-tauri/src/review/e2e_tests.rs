//! End-to-end: queue -> fetch -> worktree -> (fake) Claude -> postprocess -> store ->
//! auto-approve -> posting. Uses a real local git "forge" and an in-memory provider.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use crate::db::Db;
use crate::db::models::*;
use crate::error::AppResult;
use crate::providers::{GitProvider, PostResult, PrFilter, ReviewEvent, ReviewSubmission};
use crate::review::post::{PostRequest, SubmitMode, post_findings};
use crate::review::queue;
use crate::secrets::{MemoryStore, SecretStore};
use crate::service::Service;
use crate::service::testing::RecordingSink;
use crate::settings::{AppConfig, SettingsOverrides};

#[derive(Default)]
struct Recorded {
    submissions: Vec<ReviewSubmission>,
    approvals: Vec<String>,
}

struct FakeProvider {
    pr: PullRequest,
    recorded: Arc<Mutex<Recorded>>,
}

#[async_trait]
impl GitProvider for FakeProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Github
    }
    async fn current_user(&self) -> AppResult<String> {
        Ok("me".into())
    }
    async fn list_open_prs(&self, _f: PrFilter) -> AppResult<Vec<PullRequest>> {
        Ok(vec![self.pr.clone()])
    }
    async fn get_pr(&self, _p: &str, _n: u64) -> AppResult<PullRequest> {
        Ok(self.pr.clone())
    }
    async fn post_review(&self, _pr: &PullRequest, s: &ReviewSubmission) -> AppResult<PostResult> {
        self.recorded.lock().unwrap().submissions.push(s.clone());
        Ok(PostResult {
            posted: s
                .inline
                .iter()
                .map(|c| (c.finding_id.clone(), None))
                .collect(),
            folded: vec![],
            review_url: Some("https://forge/review/1".into()),
        })
    }
    async fn approve(&self, _pr: &PullRequest, body: &str) -> AppResult<()> {
        self.recorded.lock().unwrap().approvals.push(body.into());
        Ok(())
    }
    fn git_auth_header(&self) -> String {
        "Authorization: Basic dGVzdA==".into()
    }
    fn pr_fetch_ref(&self, n: u64) -> String {
        format!("refs/pull/{n}/head")
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// origin repo with `main` and a PR ref `refs/pull/9/head` that changes line 2 of lib.rs.
fn make_forge(root: &Path) -> (PathBuf, String, String) {
    let origin = root.join("origin");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "-q", "-b", "main"]);
    std::fs::write(origin.join("lib.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
    git(&origin, &["add", "."]);
    git(&origin, &["commit", "-qm", "base"]);
    let base = git(&origin, &["rev-parse", "HEAD"]);
    git(&origin, &["checkout", "-qb", "feature"]);
    std::fs::write(
        origin.join("lib.rs"),
        "fn a() {}\nfn b() { unsafe_thing() }\nfn c() {}\n",
    )
    .unwrap();
    // A malicious PR could add hooks; the reviewer must never load them.
    std::fs::create_dir_all(origin.join(".claude")).unwrap();
    std::fs::write(origin.join(".claude/settings.json"), r#"{"hooks":{}}"#).unwrap();
    git(&origin, &["add", "."]);
    git(&origin, &["commit", "-qm", "make b unsafe"]);
    let head = git(&origin, &["rev-parse", "HEAD"]);
    git(&origin, &["update-ref", "refs/pull/9/head", &head]);
    git(&origin, &["checkout", "-q", "main"]);
    (origin, base, head)
}

#[cfg(unix)]
fn write_fake_claude(dir: &Path, verdict: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let output = serde_json::json!({
        "verdict": verdict,
        "summary": "One small concern — otherwise solid.\n\n🤖 Generated with Claude Code",
        "findings": [
            {
                "severity": "low", "category": "maintainability", "title": "Explain unsafe_thing",
                "file": "lib.rs", "line_start": 2, "line_end": null, "side": "right",
                "explanation": "Undocumented call.",
                "proposed_comment": "Please document why this is needed -- callers can't tell.\n\nCo-Authored-By: Claude <noreply@anthropic.com>",
                "suggestion": null
            },
            {
                "severity": "nit", "category": "style", "title": "Spacing",
                "file": "lib.rs", "line_start": 1, "line_end": null, "side": "right",
                "explanation": "x", "proposed_comment": "nit", "suggestion": null
            }
        ]
    });
    let stream = [
        serde_json::json!({"type":"system","subtype":"init","session_id":"s","model":"claude-test"}),
        serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"lib.rs"}}]}}),
        serde_json::json!({"type":"result","subtype":"success","is_error":false,"session_id":"s","total_cost_usd":0.12,"structured_output":output}),
    ]
    .iter()
    .map(|v| v.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    std::fs::write(dir.join("stream.jsonl"), stream).unwrap();
    let script = dir.join("claude");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\npwd > {d}/cwd.txt\necho \"$@\" > {d}/args.txt\ncat > {d}/prompt.txt\ncat {d}/stream.jsonl\n",
            d = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

struct Harness {
    svc: Arc<Service>,
    sink: Arc<RecordingSink>,
    recorded: Arc<Mutex<Recorded>>,
    pr: PullRequest,
    tmp: tempfile::TempDir,
}

#[cfg(unix)]
fn harness(verdict: &str, overrides: SettingsOverrides) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let (origin, base, head) = make_forge(tmp.path());
    let claude = write_fake_claude(tmp.path(), verdict);

    let db = Arc::new(Db::open_in_memory().unwrap());
    db.set_app_config(&AppConfig {
        claude_path: Some(claude.to_string_lossy().into()),
        ..Default::default()
    })
    .unwrap();
    let server = db
        .insert_server(&ServerInput {
            kind: ProviderKind::Github,
            name: "fake".into(),
            base_url: "https://forge".into(),
            pr_scope: None,
            token: None,
        })
        .unwrap();
    db.set_server_settings(&server.id, &overrides).unwrap();
    let secrets = Arc::new(MemoryStore::default());
    secrets.set(&server.id, "token").unwrap();

    let pr = PullRequest {
        server_id: server.id.clone(),
        repo: "acme/app".into(),
        project_id: "acme/app".into(),
        number: 9,
        title: "Make b unsafe".into(),
        description: "desc".into(),
        author: "bob".into(),
        web_url: "https://forge/acme/app/pull/9".into(),
        clone_url: Some(origin.to_string_lossy().into()),
        head_sha: head,
        base_sha: Some(base),
        start_sha: None,
        head_ref: "feature".into(),
        base_ref: "main".into(),
        draft: false,
        state: PrState::Open,
        updated_at: String::new(),
        review_requested: true,
        authored_by_me: false,
    };
    let recorded = Arc::new(Mutex::new(Recorded::default()));
    let sink = Arc::new(RecordingSink::default());
    let (pr2, rec2) = (pr.clone(), recorded.clone());
    let svc = Service::new(
        db,
        secrets,
        tmp.path().join("cache"),
        sink.clone(),
        Some(Box::new(move |_s, _t, _u| {
            Arc::new(FakeProvider {
                pr: pr2.clone(),
                recorded: rec2.clone(),
            }) as Arc<dyn GitProvider>
        })),
    )
    .unwrap();
    Harness {
        svc,
        sink,
        recorded,
        pr,
        tmp,
    }
}

async fn wait_finished(svc: &Service, review_id: &str) -> Review {
    for _ in 0..200 {
        let r = svc.db.get_review(review_id).unwrap();
        if r.status.is_finished() && !svc.queue.is_active(&r.server_id, &r.repo, r.number) {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("review did not finish");
}

#[cfg(unix)]
#[tokio::test]
async fn full_review_and_post_flow() {
    let h = harness(
        "comment",
        SettingsOverrides {
            skip_nits: Some(true),
            comment_language: Some("German".into()),
            default_selected_min_severity: Some(Severity::Low),
            ..Default::default()
        },
    );
    let review = queue::enqueue(&h.svc, h.pr.clone(), ReviewTrigger::Manual).unwrap();
    let review = wait_finished(&h.svc, &review.id).await;
    assert_eq!(
        review.status,
        ReviewStatus::Done,
        "error: {:?}",
        review.error
    );
    assert_eq!(review.verdict, Some(Verdict::Comment));
    assert_eq!(review.cost_usd, Some(0.12));
    assert_eq!(
        review.summary.as_deref(),
        Some("One small concern, otherwise solid.")
    );

    // The reviewer ran inside the worktree, locked down, with the German instruction.
    let dir = h.tmp.path();
    let args = std::fs::read_to_string(dir.join("args.txt")).unwrap();
    assert!(
        args.contains("--setting-sources user") && args.contains("--tools Read Grep Glob Skill")
    );
    assert!(args.contains("Write them in German"));
    let cwd = std::fs::read_to_string(dir.join("cwd.txt")).unwrap();
    assert!(cwd.trim().ends_with("worktree"));
    let prompt = std::fs::read_to_string(dir.join("prompt.txt")).unwrap();
    assert!(prompt.contains("acme/app#9") && prompt.contains("pr.diff"));
    // Worktree cleaned up afterwards.
    assert!(!h.svc.cache.review_dir(&review.id).exists());

    // Nit skipped, comment sanitized, anchored in the diff, pre-selected.
    let findings = h.svc.db.list_findings(&review.id).unwrap();
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    assert_eq!(
        f.proposed_comment,
        "Please document why this is needed, callers can't tell."
    );
    assert!(f.in_diff && f.selected);

    // Tracking updated, user notified.
    let t = h
        .svc
        .db
        .get_tracked(&h.pr.server_id, "acme/app", 9)
        .unwrap()
        .unwrap();
    assert_eq!(t.last_verdict, Some(Verdict::Comment));
    assert!(!t.has_new_changes);
    assert!(
        h.sink
            .notifications
            .lock()
            .unwrap()
            .iter()
            .any(|(t, _)| t.contains("Review finished"))
    );
    assert!(h.recorded.lock().unwrap().approvals.is_empty());

    // User edits the comment (with a forbidden em dash) and posts it.
    h.svc
        .db
        .update_finding(&f.id, true, "Bitte dokumentieren — warum ist das nötig?")
        .unwrap();
    let outcome = post_findings(
        &h.svc,
        PostRequest {
            review_id: review.id.clone(),
            finding_ids: vec![f.id.clone()],
            mode: SubmitMode::Comment,
            summary: Some("Kurz: ein Punkt.".into()),
        },
    )
    .await
    .unwrap();
    assert_eq!(outcome.posted, 1);
    let rec = h.recorded.lock().unwrap();
    let sub = &rec.submissions[0];
    assert_eq!(sub.event, ReviewEvent::Comment);
    assert_eq!(sub.body, "Kurz: ein Punkt.");
    assert_eq!(sub.inline.len(), 1);
    assert_eq!(sub.inline[0].path, "lib.rs");
    assert_eq!(sub.inline[0].line, 2);
    assert_eq!(
        sub.inline[0].body,
        "Bitte dokumentieren, warum ist das nötig?"
    );
    drop(rec);
    assert!(h.svc.db.get_finding(&f.id).unwrap().posted_at.is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn approve_verdict_with_only_low_findings_auto_approves() {
    let h = harness(
        "approve",
        SettingsOverrides {
            auto_approve_on_approve_verdict: Some(true),
            ..Default::default()
        },
    );
    let review = queue::enqueue(&h.svc, h.pr.clone(), ReviewTrigger::Auto).unwrap();
    let review = wait_finished(&h.svc, &review.id).await;
    assert_eq!(
        review.verdict,
        Some(Verdict::Approve),
        "error: {:?}",
        review.error
    );
    let review = h.svc.db.get_review(&review.id).unwrap();
    assert!(review.approval_posted);

    let rec = h.recorded.lock().unwrap();
    assert_eq!(
        rec.approvals,
        vec!["One small concern, otherwise solid.".to_string()]
    );
    assert!(
        rec.submissions.is_empty(),
        "auto-approve must not post the findings"
    );
    drop(rec);
    assert!(
        h.sink
            .notifications
            .lock()
            .unwrap()
            .iter()
            .any(|(t, _)| t == "Approved acme/app#9")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn same_head_is_deduplicated_while_queued() {
    let h = harness("comment", SettingsOverrides::default());
    let a = queue::enqueue(&h.svc, h.pr.clone(), ReviewTrigger::Manual).unwrap();
    let b = queue::enqueue(&h.svc, h.pr.clone(), ReviewTrigger::Auto).unwrap();
    assert_eq!(a.id, b.id);
    wait_finished(&h.svc, &a.id).await;
}

/// Runs the real `claude` CLI (costs a few cents). `cargo test real_claude -- --ignored`
#[cfg(unix)]
#[tokio::test]
#[ignore]
async fn real_claude_review() {
    let h = harness(
        "unused",
        SettingsOverrides {
            model: Some("haiku".into()),
            comment_language: Some("German".into()),
            ..Default::default()
        },
    );
    h.svc.db.set_app_config(&AppConfig::default()).unwrap();
    let review = queue::enqueue(&h.svc, h.pr.clone(), ReviewTrigger::Manual).unwrap();
    let mut review = review;
    for _ in 0..600 {
        review = h.svc.db.get_review(&review.id).unwrap();
        if review.status.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(
        review.status,
        ReviewStatus::Done,
        "error: {:?}",
        review.error
    );
    let findings = h.svc.db.list_findings(&review.id).unwrap();
    println!("verdict={:?} summary={:?}", review.verdict, review.summary);
    for f in &findings {
        println!(
            "[{:?}] {} {:?}:{:?} in_diff={} :: {}",
            f.severity, f.title, f.file, f.line_start, f.in_diff, f.proposed_comment
        );
        assert!(crate::review::postprocess::dash_violations(&f.proposed_comment).is_empty());
    }
    for line in h.svc.progress_log(&review.id) {
        println!("  {:?} {}", line.kind, line.message);
    }
}

fn dummy_pr() -> PullRequest {
    PullRequest {
        server_id: String::new(),
        repo: "acme/app".into(),
        project_id: "acme/app".into(),
        number: 1,
        title: String::new(),
        description: String::new(),
        author: String::new(),
        web_url: String::new(),
        clone_url: None,
        head_sha: String::new(),
        base_sha: None,
        start_sha: None,
        head_ref: String::new(),
        base_ref: String::new(),
        draft: false,
        state: PrState::Open,
        updated_at: String::new(),
        review_requested: false,
        authored_by_me: false,
    }
}

fn service_with(db: Arc<Db>, secrets: Arc<dyn SecretStore>, cache: PathBuf) -> Arc<Service> {
    Service::new(
        db,
        secrets,
        cache,
        Arc::new(RecordingSink::default()),
        Some(Box::new(|_s, _t, _u| {
            Arc::new(FakeProvider {
                pr: dummy_pr(),
                recorded: Arc::default(),
            }) as Arc<dyn GitProvider>
        })),
    )
    .unwrap()
}

fn gitlab_input(token: Option<&str>) -> ServerInput {
    ServerInput {
        kind: ProviderKind::Gitlab,
        name: "Corp".into(),
        base_url: "https://gitlab.corp".into(),
        pr_scope: None,
        token: token.map(Into::into),
    }
}

/// Every byte the database wrote to disk (main file, WAL and shared memory).
fn db_bytes(dir: &Path) -> Vec<u8> {
    let mut all = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            all.extend(std::fs::read(path).unwrap());
        }
    }
    all
}

#[tokio::test]
async fn tokens_live_in_the_secret_store_never_in_the_database() {
    const TOKEN: &str = "glpat-SUPERSECRET-0123456789";
    const ROTATED: &str = "glpat-ROTATED-9876543210";
    let tmp = tempfile::tempdir().unwrap();
    let db_dir = tmp.path().join("data");
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = Arc::new(Db::open(&db_dir.join("pr-helper.sqlite3")).unwrap());
    let secrets = Arc::new(MemoryStore::default());
    let svc = service_with(db, secrets.clone(), tmp.path().join("cache"));

    let server = svc
        .save_server(None, &gitlab_input(Some(TOKEN)))
        .await
        .unwrap();
    assert_eq!(server.username.as_deref(), Some("me"));
    assert_eq!(secrets.get(&server.id).unwrap().as_deref(), Some(TOKEN));

    // Editing without a token keeps the stored one; a new token replaces it.
    svc.save_server(Some(&server.id), &gitlab_input(None))
        .await
        .unwrap();
    assert_eq!(secrets.get(&server.id).unwrap().as_deref(), Some(TOKEN));
    svc.save_server(Some(&server.id), &gitlab_input(Some(ROTATED)))
        .await
        .unwrap();
    assert_eq!(secrets.get(&server.id).unwrap().as_deref(), Some(ROTATED));

    let bytes = db_bytes(&db_dir);
    assert!(!bytes.is_empty());
    for secret in [TOKEN, ROTATED] {
        assert!(
            !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "token found in the database files"
        );
    }
}

struct LockedKeychain;

impl SecretStore for LockedKeychain {
    fn get(&self, _id: &str) -> AppResult<Option<String>> {
        Ok(None)
    }
    fn set(&self, _id: &str, _token: &str) -> AppResult<()> {
        Err(crate::error::AppError::Secret("keychain is locked".into()))
    }
    fn delete(&self, _id: &str) -> AppResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn a_failed_keychain_write_does_not_leave_a_server_behind() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open_in_memory().unwrap());
    let svc = service_with(
        db.clone(),
        Arc::new(LockedKeychain),
        tmp.path().join("cache"),
    );
    let err = svc
        .save_server(None, &gitlab_input(Some("glpat-x")))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("keychain is locked"));
    assert!(db.list_servers().unwrap().is_empty());
}
