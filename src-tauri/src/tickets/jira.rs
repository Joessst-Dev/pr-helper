//! Jira REST API v2 client (Cloud and Server/Data Center).
//!
//! v2 returns rich text as plain/wiki strings on both flavours, so no ADF parsing is needed.

use reqwest::StatusCode;
use serde_json::Value;

use super::{MAX_COMMENTS, Ticket, TicketComment, browse_url, is_valid_key, truncate};
use crate::error::{AppError, AppResult};

const NAME: &str = "Jira";
const MAX_DESCRIPTION: usize = 8_000;
const MAX_FIELD: usize = 4_000;
const MAX_COMMENT: usize = 1_500;

pub enum JiraAuth {
    /// Jira Cloud: account email plus API token.
    Basic { email: String, token: String },
    /// Jira Server / Data Center: personal access token.
    Bearer(String),
}

pub struct JiraClient {
    client: reqwest::Client,
    base: String,
    auth: JiraAuth,
}

impl JiraClient {
    pub fn new(base_url: &str, auth: JiraAuth, client: reqwest::Client) -> Self {
        Self {
            client,
            base: base_url.trim().trim_end_matches('/').to_string(),
            auth,
        }
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        let req = self
            .client
            .get(format!("{}/rest/api/2{path}", self.base))
            .header("Accept", "application/json");
        match &self.auth {
            JiraAuth::Basic { email, token } => req.basic_auth(email, Some(token)),
            JiraAuth::Bearer(token) => req.bearer_auth(token),
        }
    }

    async fn send(&self, req: reqwest::RequestBuilder, what: &str) -> AppResult<Value> {
        let resp = req.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp.json().await?);
        }
        let message = match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                "Jira rejected the credentials (check the email and token)".to_string()
            }
            StatusCode::NOT_FOUND => format!("{what} was not found or is not visible to you"),
            _ => {
                let body = resp.text().await.unwrap_or_default();
                serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|v| {
                        v["errorMessages"]
                            .as_array()
                            .filter(|a| !a.is_empty())
                            .map(|a| {
                                a.iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            })
                    })
                    .unwrap_or_else(|| body.chars().take(300).collect())
            }
        };
        Err(AppError::Api {
            provider: NAME,
            status: status.as_u16(),
            message,
        })
    }

    /// The display name of the token's account; verifies URL and credentials.
    pub async fn myself(&self) -> AppResult<String> {
        let v = self.send(self.get("/myself"), "the current user").await?;
        ["displayName", "name", "emailAddress"]
            .iter()
            .find_map(|k| v[*k].as_str().filter(|s| !s.is_empty()))
            .map(str::to_string)
            .ok_or_else(|| AppError::invalid("Jira /myself returned no user name"))
    }

    pub async fn issue(&self, key: &str, extra_fields: &[String]) -> AppResult<Ticket> {
        if !is_valid_key(key) {
            return Err(AppError::invalid(format!("invalid ticket key {key:?}")));
        }
        let mut fields = vec![
            "summary",
            "issuetype",
            "status",
            "description",
            "parent",
            "comment",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        fields.extend(extra_fields.iter().cloned());
        let req = self.get(&format!("/issue/{key}")).query(&[
            ("fields", fields.join(",")),
            ("expand", "names".to_string()),
        ]);
        let v = self.send(req, &format!("Ticket {key}")).await?;
        Ok(parse_issue(&v, key, extra_fields, &self.base))
    }
}

/// Comma separated field ids → validated list (`customfield_10035`, `labels`, ...).
pub fn parse_extra_fields(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split([',', ' ', '\n'])
        .map(str::trim)
        .filter(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .map(str::to_string)
        .collect()
}

fn parse_issue(v: &Value, key: &str, extra_fields: &[String], base: &str) -> Ticket {
    let f = &v["fields"];
    let text = |v: &Value| v.as_str().unwrap_or_default().trim().to_string();
    let parent = f["parent"]["key"].as_str().map(|k| {
        let summary = text(&f["parent"]["fields"]["summary"]);
        if summary.is_empty() {
            k.to_string()
        } else {
            format!("{k}: {summary}")
        }
    });
    let fields = extra_fields
        .iter()
        .filter_map(|id| {
            let value = field_text(&f[id.as_str()]);
            (!value.is_empty()).then(|| {
                let name = v["names"][id.as_str()]
                    .as_str()
                    .unwrap_or(id.as_str())
                    .to_string();
                (name, truncate(&value, MAX_FIELD))
            })
        })
        .collect();
    let all_comments = f["comment"]["comments"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let comment_total = f["comment"]["total"]
        .as_u64()
        .map_or(all_comments.len(), |t| t as usize)
        .max(all_comments.len());
    let comments = all_comments
        .iter()
        .skip(all_comments.len().saturating_sub(MAX_COMMENTS))
        .map(|c| TicketComment {
            author: ["displayName", "name"]
                .iter()
                .find_map(|k| c["author"][*k].as_str())
                .unwrap_or("unknown")
                .to_string(),
            created: c["created"].as_str().unwrap_or_default().to_string(),
            body: truncate(&field_text(&c["body"]), MAX_COMMENT),
        })
        .collect();
    Ticket {
        key: key.to_string(),
        url: browse_url(base, key),
        summary: text(&f["summary"]),
        issue_type: text(&f["issuetype"]["name"]),
        status: text(&f["status"]["name"]),
        description: truncate(&field_text(&f["description"]), MAX_DESCRIPTION),
        parent,
        fields,
        comments,
        comment_total,
    }
}

/// Best-effort plain text for any Jira field value.
fn field_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(items) => items
            .iter()
            .map(field_text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(o) => {
            // Atlassian Document Format (v3 style values that sometimes leak into v2).
            if o.get("type").and_then(Value::as_str) == Some("doc") {
                let mut out = String::new();
                adf_text(v, &mut out);
                return out.trim().to_string();
            }
            ["value", "name", "displayName", "key"]
                .iter()
                .find_map(|k| o.get(*k).and_then(Value::as_str))
                .unwrap_or_default()
                .trim()
                .to_string()
        }
    }
}

fn adf_text(node: &Value, out: &mut String) {
    if let Some(t) = node["text"].as_str() {
        out.push_str(t);
    }
    for child in node["content"].as_array().into_iter().flatten() {
        adf_text(child, out);
    }
    if matches!(
        node["type"].as_str(),
        Some("paragraph" | "heading" | "listItem" | "codeBlock" | "hardBreak")
    ) {
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer, auth: JiraAuth) -> JiraClient {
        JiraClient::new(
            &format!("{}/", server.uri()),
            auth,
            crate::providers::http_client(),
        )
    }

    fn issue_json() -> Value {
        let comments: Vec<Value> = (1..=12)
            .map(|i| {
                serde_json::json!({
                    "author": {"displayName": format!("User {i}")},
                    "created": format!("2026-01-{i:02}T10:00:00.000+0000"),
                    "body": format!("comment {i}")
                })
            })
            .collect();
        serde_json::json!({
            "key": "ABC-12",
            "names": {"customfield_10035": "Acceptance Criteria"},
            "fields": {
                "summary": "Users can reset their password",
                "issuetype": {"name": "Story"},
                "status": {"name": "In Progress"},
                "description": "As a user I want to reset my password.",
                "parent": {"key": "ABC-1", "fields": {"summary": "Account epic"}},
                "customfield_10035": "* Reset link expires after 1 hour",
                "comment": {"total": 12, "comments": comments}
            }
        })
    }

    #[tokio::test]
    async fn cloud_issue_uses_basic_auth_and_parses_fields() {
        let server = MockServer::start().await;
        // me@acme.test:tok
        Mock::given(method("GET"))
            .and(path("/rest/api/2/issue/ABC-12"))
            .and(header("authorization", "Basic bWVAYWNtZS50ZXN0OnRvaw=="))
            .and(query_param("expand", "names"))
            .and(query_param(
                "fields",
                "summary,issuetype,status,description,parent,comment,customfield_10035",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(issue_json()))
            .expect(1)
            .mount(&server)
            .await;
        let jira = client(
            &server,
            JiraAuth::Basic {
                email: "me@acme.test".into(),
                token: "tok".into(),
            },
        );
        let t = jira
            .issue("ABC-12", &["customfield_10035".into()])
            .await
            .unwrap();
        assert_eq!(t.summary, "Users can reset their password");
        assert_eq!(t.issue_type, "Story");
        assert_eq!(t.status, "In Progress");
        assert_eq!(t.parent.as_deref(), Some("ABC-1: Account epic"));
        assert_eq!(
            t.fields,
            vec![(
                "Acceptance Criteria".to_string(),
                "* Reset link expires after 1 hour".to_string()
            )]
        );
        assert_eq!(t.comments.len(), MAX_COMMENTS);
        assert_eq!(t.comments[0].body, "comment 3");
        assert_eq!(t.comments[9].author, "User 12");
        assert_eq!(t.comment_total, 12);
        assert_eq!(
            t.url.as_deref(),
            Some(format!("{}/browse/ABC-12", server.uri()).as_str())
        );
    }

    #[tokio::test]
    async fn server_uses_bearer_and_reports_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/2/myself"))
            .and(header("authorization", "Bearer pat"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"name": "jdoe"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/2/issue/ABC-404"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/2/issue/ABC-500"))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_json(serde_json::json!({"errorMessages": ["boom"]})),
            )
            .mount(&server)
            .await;
        let jira = client(&server, JiraAuth::Bearer("pat".into()));
        assert_eq!(jira.myself().await.unwrap(), "jdoe");
        let err = jira.issue("ABC-404", &[]).await.unwrap_err().to_string();
        assert!(err.contains("not found"), "{err}");
        let err = jira.issue("ABC-500", &[]).await.unwrap_err().to_string();
        assert!(err.contains("boom"), "{err}");
        // Never sent: the key is not a valid ticket key.
        assert!(jira.issue("ABC-1/../../x", &[]).await.is_err());
    }

    #[tokio::test]
    async fn bad_credentials_are_explained() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/2/myself"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let jira = client(&server, JiraAuth::Bearer("wrong".into()));
        let err = jira.myself().await.unwrap_err().to_string();
        assert!(err.contains("rejected the credentials"), "{err}");
        assert!(!err.contains("wrong"));
    }

    #[test]
    fn field_text_handles_common_shapes() {
        let v = serde_json::json!([{"value": "Web"}, {"name": "API"}, null, "x"]);
        assert_eq!(field_text(&v), "Web, API, x");
        let adf = serde_json::json!({"type": "doc", "content": [
            {"type": "paragraph", "content": [{"type": "text", "text": "Given a user"}]},
            {"type": "paragraph", "content": [{"type": "text", "text": "Then reset works"}]}
        ]});
        assert_eq!(field_text(&adf), "Given a user\nThen reset works");
        assert_eq!(
            parse_extra_fields(Some("customfield_1, labels ,bad/field")),
            vec!["customfield_1", "labels"]
        );
    }
}
