//! Spawns a headless Claude Code reviewer and parses its stream-json output.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::error::{AppError, AppResult};

/// How long the CLI may take to exit after its final result line.
const EXIT_GRACE: Duration = Duration::from_secs(10);

/// Built-in tools the reviewer gets. No Bash, no writes, no network.
pub const REVIEWER_TOOLS: &[&str] = &["Read", "Grep", "Glob", "Skill"];

/// Which tools a run may use.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolPolicy {
    /// The PR reviewer: read-only built-ins, no MCP servers at all.
    Reviewer,
    /// The ticket fetcher: no built-in tools, only the named MCP tools
    /// (`mcp__<server>__<tool>`). It never sees PR content and runs in an empty directory.
    /// User-scoped MCP servers must load, so there is no `--strict-mcp-config`.
    TicketFetcher { allowed: Vec<String> },
}

pub struct RunRequest {
    pub policy: ToolPolicy,
    pub claude: PathBuf,
    pub path_env: String,
    pub cwd: PathBuf,
    pub add_dirs: Vec<PathBuf>,
    pub plugin_dir: Option<PathBuf>,
    pub system_prompt: String,
    pub prompt: String,
    pub schema: Value,
    pub session_id: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub max_budget_usd: Option<f64>,
    pub timeout: Duration,
}

impl RunRequest {
    pub fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec![
            "-p".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--json-schema".into(),
            self.schema.to_string(),
            "--session-id".into(),
            self.session_id.clone(),
            "--append-system-prompt".into(),
            self.system_prompt.clone(),
            // The checkout is untrusted PR content: ignore its .claude/settings.json (hooks!).
            "--setting-sources".into(),
            "user".into(),
        ];
        match &self.policy {
            ToolPolicy::Reviewer => {
                a.extend(["--permission-mode".into(), "default".into()]);
                // ...and any MCP servers it declares.
                a.push("--strict-mcp-config".into());
                a.push("--tools".into());
                a.extend(REVIEWER_TOOLS.iter().map(ToString::to_string));
                a.push("--allowed-tools".into());
                a.extend(REVIEWER_TOOLS.iter().map(ToString::to_string));
            }
            ToolPolicy::TicketFetcher { allowed } => {
                // Every other loaded MCP tool (all of the user's servers and connectors are
                // visible) is denied without a prompt. Verified with the real CLI, including
                // calls routed through a gateway server.
                a.extend(["--permission-mode".into(), "dontAsk".into()]);
                // No built-in tools at all (no Read, Bash, web, ...).
                a.push("--tools".into());
                a.push(String::new());
                a.push("--allowed-tools".into());
                a.extend(allowed.iter().cloned());
            }
        }
        for d in &self.add_dirs {
            a.push("--add-dir".into());
            a.push(d.to_string_lossy().into_owned());
        }
        if let Some(p) = &self.plugin_dir {
            a.push("--plugin-dir".into());
            a.push(p.to_string_lossy().into_owned());
        }
        if let Some(m) = self.model.as_deref().filter(|m| !m.is_empty()) {
            a.push("--model".into());
            a.push(m.into());
        }
        if let Some(e) = self.effort.as_deref().filter(|e| !e.is_empty()) {
            a.push("--effort".into());
            a.push(e.into());
        }
        if let Some(b) = self.max_budget_usd.filter(|b| *b > 0.0) {
            a.push("--max-budget-usd".into());
            a.push(format!("{b}"));
        }
        a
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunEvent {
    Started { session_id: String, model: String },
    Tool { name: String, detail: String },
    Text(String),
}

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub session_id: Option<String>,
    pub output: Value,
    pub cost_usd: Option<f64>,
}

/// Parse one stream-json line into UI events. The final `result` line is returned separately.
pub fn parse_line(line: &str) -> (Vec<RunEvent>, Option<Value>) {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return (Vec::new(), None);
    };
    let mut events = Vec::new();
    match v["type"].as_str() {
        Some("system") if v["subtype"] == "init" => events.push(RunEvent::Started {
            session_id: v["session_id"].as_str().unwrap_or_default().into(),
            model: v["model"].as_str().unwrap_or_default().into(),
        }),
        Some("assistant") => {
            for item in v["message"]["content"].as_array().into_iter().flatten() {
                match item["type"].as_str() {
                    Some("text") => {
                        let t = item["text"].as_str().unwrap_or_default().trim();
                        if !t.is_empty() {
                            events.push(RunEvent::Text(t.to_string()));
                        }
                    }
                    Some("tool_use") => {
                        let name = item["name"].as_str().unwrap_or("tool").to_string();
                        if name == "StructuredOutput" {
                            continue;
                        }
                        events.push(RunEvent::Tool {
                            detail: tool_detail(&name, &item["input"]),
                            name,
                        });
                    }
                    _ => {}
                }
            }
        }
        Some("result") => return (events, Some(v)),
        _ => {}
    }
    (events, None)
}

fn tool_detail(name: &str, input: &Value) -> String {
    let s = |k: &str| input[k].as_str().unwrap_or_default().to_string();
    match name {
        "Read" => s("file_path"),
        "Grep" => {
            let path = s("path");
            if path.is_empty() {
                s("pattern")
            } else {
                format!("{} in {path}", s("pattern"))
            }
        }
        "Glob" => s("pattern"),
        "Skill" => {
            let skill = s("skill");
            if skill.is_empty() {
                s("command")
            } else {
                skill
            }
        }
        _ => String::new(),
    }
}

/// Interpret the final `result` message.
pub fn parse_result(v: &Value) -> AppResult<RunOutcome> {
    let session_id = v["session_id"].as_str().map(str::to_string);
    let cost_usd = v["total_cost_usd"].as_f64();
    if v["is_error"].as_bool().unwrap_or(false) || v["subtype"] != "success" {
        let reason = v["result"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| v["subtype"].as_str())
            .unwrap_or("unknown error");
        return Err(AppError::Reviewer(format!("Claude run failed: {reason}")));
    }
    let output = match &v["structured_output"] {
        Value::Object(_) => v["structured_output"].clone(),
        // Older CLIs only put the JSON text in `result`.
        _ => v["result"].as_str().and_then(extract_json).ok_or_else(|| {
            AppError::Reviewer("the reviewer returned no structured output".into())
        })?,
    };
    Ok(RunOutcome {
        session_id,
        output,
        cost_usd,
    })
}

fn extract_json(text: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return v.is_object().then_some(v);
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(&text[start..=end]).ok()
}

pub async fn run(
    req: RunRequest,
    cancel: CancellationToken,
    mut on_event: impl FnMut(RunEvent) + Send,
) -> AppResult<RunOutcome> {
    let mut child = Command::new(&req.claude)
        .args(req.args())
        .current_dir(&req.cwd)
        .env("PATH", &req.path_env)
        // Keep the reviewer from inheriting our own Claude Code session when run from a terminal.
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            AppError::Reviewer(format!("failed to start {}: {e}", req.claude.display()))
        })?;

    let mut stdin = child.stdin.take().expect("piped stdin");
    let prompt = req.prompt.clone();
    tokio::spawn(async move {
        let _ = stdin.write_all(prompt.as_bytes()).await;
        let _ = stdin.shutdown().await;
    });

    let mut stderr = child.stderr.take().expect("piped stderr");
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        let text = String::from_utf8_lossy(&buf).into_owned();
        let tail_start = text.len().saturating_sub(4000);
        text[text.floor_char_boundary(tail_start)..].to_string()
    });

    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();

    let read_all = async {
        let mut result = None;
        while let Some(line) = lines.next_line().await? {
            let (events, res) = parse_line(&line);
            for e in events {
                on_event(e);
            }
            // The result is the last thing that matters; don't let a CLI that lingers after
            // it turn a finished review into a timeout.
            if res.is_some() {
                result = res;
                break;
            }
        }
        Ok::<_, std::io::Error>(result)
    };

    let result = tokio::select! {
        r = tokio::time::timeout(req.timeout, read_all) => match r {
            Ok(r) => r?,
            Err(_) => {
                let _ = child.kill().await;
                return Err(AppError::Reviewer(format!(
                    "review timed out after {} minutes", req.timeout.as_secs() / 60
                )));
            }
        },
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return Err(AppError::Reviewer("cancelled".into()));
        }
    };

    let status = match tokio::time::timeout(EXIT_GRACE, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            let _ = child.kill().await;
            child.wait().await?
        }
    };
    let stderr_tail = tokio::time::timeout(EXIT_GRACE, stderr_task)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    match result {
        Some(v) => parse_result(&v),
        None => Err(AppError::Reviewer(format!(
            "claude exited ({status}) without a result. {}",
            stderr_tail.trim()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/claude-stream.jsonl");

    #[test]
    fn parses_recorded_stream() {
        let mut events = Vec::new();
        let mut result = None;
        for line in FIXTURE.lines() {
            let (e, r) = parse_line(line);
            events.extend(e);
            if r.is_some() {
                result = r;
            }
        }
        assert!(
            matches!(&events[0], RunEvent::Started { model, .. } if model.starts_with("claude-"))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, RunEvent::Tool { name, .. } if name == "Bash"))
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, RunEvent::Tool { name, .. } if name == "StructuredOutput"))
        );
        let outcome = parse_result(&result.unwrap()).unwrap();
        assert_eq!(outcome.output["verdict"], "request_changes");
        assert!(outcome.cost_usd.unwrap() > 0.0);
        assert!(outcome.session_id.is_some());
    }

    #[test]
    fn error_results_become_errors() {
        let v =
            serde_json::json!({"type":"result","subtype":"error_max_budget_usd","is_error":true});
        assert!(
            parse_result(&v)
                .unwrap_err()
                .to_string()
                .contains("error_max_budget_usd")
        );
    }

    #[test]
    fn falls_back_to_json_in_result_text() {
        let v = serde_json::json!({
            "type":"result","subtype":"success","is_error":false,
            "result":"Here you go:\n{\"verdict\":\"approve\",\"summary\":\"ok\",\"findings\":[]}"
        });
        assert_eq!(parse_result(&v).unwrap().output["verdict"], "approve");
    }

    #[test]
    fn args_lock_down_the_session() {
        let req = RunRequest {
            policy: ToolPolicy::Reviewer,
            claude: "claude".into(),
            path_env: String::new(),
            cwd: ".".into(),
            add_dirs: vec!["/ctx".into()],
            plugin_dir: Some("/plugin".into()),
            system_prompt: "sys".into(),
            prompt: "p".into(),
            schema: serde_json::json!({}),
            session_id: "sid".into(),
            model: Some("opus".into()),
            effort: None,
            max_budget_usd: Some(2.5),
            timeout: Duration::from_secs(60),
        };
        let args = req.args().join(" ");
        assert!(args.contains("--setting-sources user"));
        assert!(args.contains("--permission-mode default"));
        assert!(args.contains("--strict-mcp-config"));
        assert!(args.contains("--tools Read Grep Glob Skill"));
        assert!(!args.contains("Bash"));
        assert!(args.contains("--add-dir /ctx"));
        assert!(args.contains("--plugin-dir /plugin"));
        assert!(args.contains("--model opus"));
        assert!(args.contains("--max-budget-usd 2.5"));
        assert!(!args.contains("--effort"));
    }

    #[test]
    fn fetcher_args_allow_only_the_named_mcp_tools() {
        let req = RunRequest {
            policy: ToolPolicy::TicketFetcher {
                allowed: vec!["mcp__atlassian__getJiraIssue".into()],
            },
            claude: "claude".into(),
            path_env: String::new(),
            cwd: "/empty".into(),
            add_dirs: vec![],
            plugin_dir: None,
            system_prompt: "sys".into(),
            prompt: "p".into(),
            schema: serde_json::json!({}),
            session_id: "sid".into(),
            model: Some("haiku".into()),
            effort: None,
            max_budget_usd: None,
            timeout: Duration::from_secs(60),
        };
        let args = req.args();
        let tools = args.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(args[tools + 1], "");
        let allowed = args.iter().position(|a| a == "--allowed-tools").unwrap();
        assert_eq!(args[allowed + 1], "mcp__atlassian__getJiraIssue");
        let joined = args.join(" ");
        assert!(joined.contains("--setting-sources user"));
        assert!(joined.contains("--permission-mode dontAsk"));
        for forbidden in [
            "Read",
            "Grep",
            "Glob",
            "Bash",
            "Skill",
            "--add-dir",
            "--plugin-dir",
        ] {
            assert!(!joined.contains(forbidden), "{forbidden} in {joined}");
        }
    }

    /// Runs the real spawn path against a fake `claude` that replays the fixture.
    #[cfg(unix)]
    #[tokio::test]
    async fn runs_a_fake_claude_binary() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let fixture = tmp.path().join("stream.jsonl");
        std::fs::write(&fixture, FIXTURE).unwrap();
        let fake = tmp.path().join("claude");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\ncat > {0}/stdin.txt\necho \"$@\" > {0}/args.txt\ncat {1}\n",
                tmp.path().display(),
                fixture.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let req = RunRequest {
            policy: ToolPolicy::Reviewer,
            claude: fake,
            path_env: std::env::var("PATH").unwrap(),
            cwd: tmp.path().into(),
            add_dirs: vec![],
            plugin_dir: None,
            system_prompt: "sys".into(),
            prompt: "review this please".into(),
            schema: serde_json::json!({"type":"object"}),
            session_id: "sid".into(),
            model: None,
            effort: None,
            max_budget_usd: None,
            timeout: Duration::from_secs(20),
        };
        let mut events = Vec::new();
        let outcome = run(req, CancellationToken::new(), |e| events.push(e))
            .await
            .unwrap();
        assert_eq!(outcome.output["verdict"], "request_changes");
        assert!(!events.is_empty());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("stdin.txt")).unwrap(),
            "review this please"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_kills_the_process() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("claude");
        std::fs::write(&fake, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let req = RunRequest {
            policy: ToolPolicy::Reviewer,
            claude: fake,
            path_env: std::env::var("PATH").unwrap(),
            cwd: tmp.path().into(),
            add_dirs: vec![],
            plugin_dir: None,
            system_prompt: String::new(),
            prompt: String::new(),
            schema: serde_json::json!({}),
            session_id: "sid".into(),
            model: None,
            effort: None,
            max_budget_usd: None,
            timeout: Duration::from_secs(60),
        };
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            c2.cancel();
        });
        let started = std::time::Instant::now();
        let err = run(req, cancel, |_| {}).await.unwrap_err();
        assert!(err.to_string().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
