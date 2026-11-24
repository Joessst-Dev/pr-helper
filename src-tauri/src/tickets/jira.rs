//! Jira REST API v2 client (Cloud and Server/Data Center).
//!
//! v2 returns rich text as plain/wiki strings on both flavours, so no ADF parsing is needed.

use reqwest::StatusCode;
use serde_json::Value;

use super::{
    MAX_COMMENT, MAX_COMMENTS, MAX_DESCRIPTION, MAX_FIELD, Ticket, TicketAttachment, TicketComment,
    TicketTextFormat, browse_url, is_displayable_image, is_valid_key, truncate,
};
use crate::error::{AppError, AppResult};

const NAME: &str = "Jira";
/// Largest image attachment the app downloads for display.
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_ATTACHMENTS: usize = 50;

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
        self.authed(
            self.client
                .get(format!("{}/rest/api/2{path}", self.base))
                .header("Accept", "application/json"),
        )
    }

    /// Adds the credentials. Only for URLs on the configured Jira site.
    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
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
        let fields: Vec<&str> = [
            "summary",
            "issuetype",
            "status",
            "description",
            "parent",
            "comment",
            "attachment",
        ]
        .into_iter()
        .chain(extra_fields.iter().map(String::as_str))
        .collect();
        let req = self.get(&format!("/issue/{key}")).query(&[
            ("fields", fields.join(",")),
            ("expand", "names".to_string()),
        ]);
        let v = self.send(req, &format!("Ticket {key}")).await?;
        Ok(parse_issue(&v, key, extra_fields, &self.base))
    }

    /// Downloads an image attachment of ticket `key`: `(mime type, bytes)`. The attachment must
    /// belong to that ticket. Only displayable image types up to [`MAX_IMAGE_BYTES`], and the
    /// credentials only ever go to the configured Jira host.
    pub async fn attachment_image(&self, key: &str, id: &str) -> AppResult<(String, Vec<u8>)> {
        if !is_valid_key(key) {
            return Err(AppError::invalid(format!("invalid ticket key {key:?}")));
        }
        if id.is_empty() || id.len() > 20 || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(AppError::invalid(format!("invalid attachment id {id:?}")));
        }
        let issue = self
            .send(
                self.get(&format!("/issue/{key}"))
                    .query(&[("fields", "attachment")]),
                &format!("Ticket {key}"),
            )
            .await?;
        let belongs = issue["fields"]["attachment"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|a| match &a["id"] {
                Value::String(s) => s == id,
                Value::Number(n) => n.to_string() == id,
                _ => false,
            });
        if !belongs {
            return Err(AppError::invalid(format!(
                "attachment {id} does not belong to {key}"
            )));
        }
        let meta = self
            .send(self.get(&format!("/attachment/{id}")), "The attachment")
            .await?;
        let mime = meta["mimeType"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !is_displayable_image(&mime) {
            return Err(AppError::invalid(format!(
                "attachment {id} is not a supported image ({mime})"
            )));
        }
        if meta["size"].as_u64().unwrap_or(0) > MAX_IMAGE_BYTES {
            return Err(AppError::invalid(format!("attachment {id} is too large")));
        }
        let content = meta["content"]
            .as_str()
            .ok_or_else(|| AppError::invalid("Jira returned no attachment URL"))?;
        let url = reqwest::Url::parse(content).map_err(|e| {
            AppError::invalid(format!("Jira returned an invalid attachment URL: {e}"))
        })?;
        let base = reqwest::Url::parse(&self.base)
            .map_err(|e| AppError::invalid(format!("the Jira URL is invalid: {e}")))?;
        if url.origin() != base.origin() {
            return Err(AppError::invalid(
                "the attachment is not hosted on the configured Jira site",
            ));
        }
        // reqwest drops the Authorization header if Jira redirects to another host (Cloud
        // serves attachment bytes from its media service with a signed URL).
        let mut resp = self.authed(self.client.get(url)).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AppError::Api {
                provider: NAME,
                status: status.as_u16(),
                message: format!("attachment {id} could not be downloaded"),
            });
        }
        if resp.content_length().unwrap_or(0) > MAX_IMAGE_BYTES {
            return Err(AppError::invalid(format!("attachment {id} is too large")));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() as u64 > MAX_IMAGE_BYTES {
                return Err(AppError::invalid(format!("attachment {id} is too large")));
            }
        }
        Ok((mime, bytes))
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
    let all_comments: &[Value] = f["comment"]["comments"]
        .as_array()
        .map_or(&[], Vec::as_slice);
    let comment_total = f["comment"]["total"]
        .as_u64()
        .map_or(all_comments.len(), |t| {
            usize::try_from(t).unwrap_or(usize::MAX)
        })
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
        text_format: TicketTextFormat::JiraWiki,
        attachments: parse_attachments(&f["attachment"]),
    }
}

/// Image attachments in Jira's order (oldest first), as the text references them by file name.
/// On tickets with very many images only the newest [`MAX_ATTACHMENTS`] are kept.
fn parse_attachments(v: &Value) -> Vec<TicketAttachment> {
    let mut images: Vec<TicketAttachment> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let id = match &a["id"] {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => return None,
            };
            let mime_type = a["mimeType"].as_str()?.to_ascii_lowercase();
            let filename = a["filename"].as_str()?.to_string();
            (is_displayable_image(&mime_type)
                && !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_digit()))
            .then(|| TicketAttachment {
                id,
                filename,
                mime_type,
                size: a["size"].as_u64().unwrap_or(0),
            })
        })
        .collect();
    let skip = images.len().saturating_sub(MAX_ATTACHMENTS);
    images.drain(..skip);
    images
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
                "comment": {"total": 12, "comments": comments},
                "attachment": [
                    {"id": "10001", "filename": "flow.png", "mimeType": "image/png", "size": 2048},
                    {"id": 10002, "filename": "spec.pdf", "mimeType": "application/pdf", "size": 1},
                    {"id": "10003", "filename": "logo.svg", "mimeType": "image/svg+xml", "size": 1},
                    {"id": "../x", "filename": "evil.png", "mimeType": "image/png", "size": 1}
                ]
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
                "summary,issuetype,status,description,parent,comment,attachment,customfield_10035",
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
        assert_eq!(t.text_format, TicketTextFormat::JiraWiki);
        // Only displayable images with a numeric id; no PDFs, no SVG.
        assert_eq!(
            t.attachments,
            vec![TicketAttachment {
                id: "10001".into(),
                filename: "flow.png".into(),
                mime_type: "image/png".into(),
                size: 2048,
            }]
        );
        assert_eq!(
            t.url.as_deref(),
            Some(format!("{}/browse/ABC-12", server.uri()).as_str())
        );
    }

    async fn mock_attachment(server: &MockServer, id: &str, meta: Value) {
        Mock::given(method("GET"))
            .and(path(format!("/rest/api/2/attachment/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(meta))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn image_attachments_download_only_from_the_jira_site() {
        let server = MockServer::start().await;
        let base = server.uri();
        mock_attachment(
            &server,
            "1",
            serde_json::json!({"mimeType": "image/png", "size": 4,
                "content": format!("{base}/secure/attachment/1/flow.png")}),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/secure/attachment/1/flow.png"))
            .and(header("authorization", "Bearer pat"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNG".to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        mock_attachment(
            &server,
            "2",
            serde_json::json!({"mimeType": "image/png", "size": 4,
                "content": "https://attacker.example/steal.png"}),
        )
        .await;
        mock_attachment(
            &server,
            "3",
            serde_json::json!({"mimeType": "text/html", "size": 4,
                "content": format!("{base}/secure/attachment/3/x.html")}),
        )
        .await;
        mock_attachment(
            &server,
            "4",
            serde_json::json!({"mimeType": "image/png", "size": MAX_IMAGE_BYTES + 1,
                "content": format!("{base}/secure/attachment/4/big.png")}),
        )
        .await;

        Mock::given(method("GET"))
            .and(path("/rest/api/2/issue/ABC-1"))
            .and(query_param("fields", "attachment"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "fields": {"attachment": [{"id": "1"}, {"id": 2}, {"id": "3"}, {"id": "4"}]}
            })))
            .mount(&server)
            .await;
        let jira = client(&server, JiraAuth::Bearer("pat".into()));
        let (mime, bytes) = jira.attachment_image("ABC-1", "1").await.unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, b"\x89PNG");
        for (id, expected) in [
            ("2", "not hosted on the configured Jira site"),
            ("3", "not a supported image"),
            ("4", "too large"),
            ("../1", "invalid attachment id"),
            ("5", "does not belong to ABC-1"),
        ] {
            let err = jira
                .attachment_image("ABC-1", id)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains(expected), "{id}: {err}");
        }
    }

    #[test]
    fn keeps_the_newest_attachments() {
        let many: Vec<Value> = (1..=MAX_ATTACHMENTS + 10)
            .map(|i| {
                serde_json::json!({"id": i.to_string(), "filename": format!("{i}.png"),
                    "mimeType": "image/png", "size": 1})
            })
            .collect();
        let kept = parse_attachments(&Value::Array(many));
        assert_eq!(kept.len(), MAX_ATTACHMENTS);
        assert_eq!(kept[0].id, "11");
        assert_eq!(kept.last().unwrap().id, (MAX_ATTACHMENTS + 10).to_string());
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
