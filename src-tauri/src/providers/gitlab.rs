//! GitLab.com and self-managed GitLab (API v4).

use std::collections::HashSet;
use std::fmt::Write as _;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{
    GitProvider, PostResult, PrFilter, ReviewEvent, ReviewSubmission, basic_auth_header, check,
    fold_comment,
};
use crate::db::models::{PrState, ProviderKind, PullRequest, Server, Side};
use crate::error::{AppError, AppResult};

const NAME: &str = "GitLab";
const MAX_PAGES: u32 = 10;

pub struct GitLab {
    server_id: String,
    api: String,
    token: String,
    username: tokio::sync::OnceCell<String>,
    groups: Vec<String>,
    client: reqwest::Client,
}

impl GitLab {
    pub fn new(
        server: &Server,
        token: String,
        username: Option<String>,
        client: reqwest::Client,
    ) -> Self {
        let base = server.base_url.trim().trim_end_matches('/');
        let api = if base.ends_with("/api/v4") {
            base.to_string()
        } else {
            format!("{base}/api/v4")
        };
        let cell = tokio::sync::OnceCell::new();
        if let Some(u) = username {
            let _ = cell.set(u);
        }
        Self {
            server_id: server.id.clone(),
            api,
            token,
            username: cell,
            groups: server
                .pr_scope
                .as_deref()
                .unwrap_or_default()
                .split([',', ' ', '\n'])
                .map(|g| g.trim().trim_start_matches("group:").to_string())
                .filter(|g| !g.is_empty())
                .collect(),
            client,
        }
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.api))
            .header("PRIVATE-TOKEN", &self.token)
    }

    async fn get_json(&self, path: &str) -> AppResult<Value> {
        let resp = self.req(reqwest::Method::GET, path).send().await?;
        Ok(check(NAME, resp).await?.json().await?)
    }

    async fn me(&self) -> AppResult<String> {
        self.username
            .get_or_try_init(|| async {
                let v = self.get_json("/user").await?;
                v["username"]
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| AppError::invalid("GitLab /user returned no username"))
            })
            .await
            .cloned()
    }

    /// Follows `x-next-page` pagination.
    async fn paged(&self, path: &str, query: &[(&str, String)]) -> AppResult<Vec<Value>> {
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let mut q: Vec<(&str, String)> = query.to_vec();
            q.push(("per_page", "100".into()));
            q.push(("page", page.to_string()));
            let resp = self
                .req(reqwest::Method::GET, path)
                .query(&q)
                .send()
                .await?;
            let resp = check(NAME, resp).await?;
            let next = resp
                .headers()
                .get("x-next-page")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse::<u32>().ok());
            let items: Vec<Value> = resp.json().await?;
            out.extend(items);
            match next {
                Some(n) if n > page && n <= MAX_PAGES => page = n,
                _ => break,
            }
        }
        Ok(out)
    }

    fn map_mr(&self, v: &Value, me: &str) -> Option<PullRequest> {
        let reference = v["references"]["full"].as_str()?;
        let repo = reference
            .rsplit_once('!')
            .map_or(reference, |(r, _)| r)
            .to_string();
        let author = v["author"]["username"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let state = match v["state"].as_str() {
            Some("merged") => PrState::Merged,
            Some("closed") | Some("locked") => PrState::Closed,
            _ => PrState::Open,
        };
        let diff_refs = &v["diff_refs"];
        Some(PullRequest {
            server_id: self.server_id.clone(),
            repo,
            project_id: v["project_id"].as_u64()?.to_string(),
            number: v["iid"].as_u64()?,
            title: v["title"].as_str().unwrap_or_default().into(),
            description: v["description"].as_str().unwrap_or_default().into(),
            web_url: v["web_url"].as_str().unwrap_or_default().into(),
            clone_url: None,
            head_sha: diff_refs["head_sha"]
                .as_str()
                .or_else(|| v["sha"].as_str())
                .unwrap_or_default()
                .into(),
            base_sha: diff_refs["base_sha"].as_str().map(str::to_string),
            start_sha: diff_refs["start_sha"].as_str().map(str::to_string),
            head_ref: v["source_branch"].as_str().unwrap_or_default().into(),
            base_ref: v["target_branch"].as_str().unwrap_or_default().into(),
            draft: v["draft"]
                .as_bool()
                .or(v["work_in_progress"].as_bool())
                .unwrap_or(false),
            state,
            updated_at: v["updated_at"].as_str().unwrap_or_default().into(),
            review_requested: v["reviewers"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|r| r["username"].as_str() == Some(me)),
            authored_by_me: author == me,
            author,
        })
    }

    fn mr_path(pr: &PullRequest) -> String {
        format!("/projects/{}/merge_requests/{}", pr.project_id, pr.number)
    }

    async fn note(&self, pr: &PullRequest, body: &str) -> AppResult<Value> {
        let resp = self
            .req(
                reqwest::Method::POST,
                &format!("{}/notes", Self::mr_path(pr)),
            )
            .json(&json!({ "body": body }))
            .send()
            .await?;
        Ok(check(NAME, resp).await?.json().await?)
    }

    fn discussion_body(c: &super::InlineComment) -> String {
        let mut body = c.body.clone();
        if let (Some(s), Side::Right) = (&c.suggestion, c.side) {
            let above = c.start_line.map_or(0, |start| c.line.saturating_sub(start));
            let fence = super::fence_for(s);
            let _ = write!(body, "\n\n{fence}suggestion:-{above}+0\n{s}\n{fence}");
        }
        body
    }
}

#[async_trait]
impl GitProvider for GitLab {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gitlab
    }

    async fn current_user(&self) -> AppResult<String> {
        self.me().await
    }

    async fn list_open_prs(&self, filter: PrFilter) -> AppResult<Vec<PullRequest>> {
        let me = self.me().await?;
        let base = [("state", "opened".to_string())];
        let mut raw: Vec<Value> = Vec::new();
        let with = |extra: &[(&'static str, String)]| {
            let mut q = base.to_vec();
            q.extend_from_slice(extra);
            q
        };
        match filter {
            PrFilter::All if !self.groups.is_empty() => {
                for g in &self.groups {
                    let path = format!("/groups/{}/merge_requests", urlencoding::encode(g));
                    raw.extend(self.paged(&path, &with(&[("scope", "all".into())])).await?);
                }
            }
            // Without a group, `scope=all` means every merge request the token can see, which
            // on gitlab.com includes every public project.
            PrFilter::All => {
                return Err(AppError::invalid(
                    "\"All visible\" needs a group scope on GitLab: list your groups (e.g. platform, team/backend) under Scope in the server's connection settings",
                ));
            }
            PrFilter::ReviewRequested => {
                raw = self
                    .paged(
                        "/merge_requests",
                        &with(&[("scope", "all".into()), ("reviewer_username", me.clone())]),
                    )
                    .await?
            }
            PrFilter::Authored => {
                raw = self
                    .paged(
                        "/merge_requests",
                        &with(&[("scope", "created_by_me".into())]),
                    )
                    .await?
            }
            PrFilter::Involved => {
                for extra in [
                    vec![("scope", "assigned_to_me".to_string())],
                    vec![("scope", "all".into()), ("reviewer_username", me.clone())],
                    vec![("scope", "created_by_me".into())],
                ] {
                    raw.extend(self.paged("/merge_requests", &with(&extra)).await?);
                }
            }
        }

        let mut seen = HashSet::new();
        let mut out: Vec<PullRequest> = raw
            .iter()
            .filter_map(|v| self.map_mr(v, &me))
            .filter(|pr| seen.insert((pr.project_id.clone(), pr.number)))
            .collect();
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    async fn get_pr(&self, project_id: &str, number: u64) -> AppResult<PullRequest> {
        // The username only decides the "requested from me"/"authored by me" flags, which are
        // not worth failing the fetch for.
        let me = self.me().await.unwrap_or_else(|e| {
            tracing::warn!("could not load the GitLab username: {e}");
            String::new()
        });
        let v = self
            .get_json(&format!("/projects/{project_id}/merge_requests/{number}"))
            .await?;
        let mut pr = self
            .map_mr(&v, &me)
            .ok_or_else(|| AppError::invalid("unexpected merge request payload"))?;
        let project = self.get_json(&format!("/projects/{project_id}")).await?;
        pr.clone_url = project["http_url_to_repo"].as_str().map(str::to_string);
        if let Some(path) = project["path_with_namespace"].as_str() {
            pr.repo = path.to_string();
        }
        Ok(pr)
    }

    async fn post_review(&self, pr: &PullRequest, sub: &ReviewSubmission) -> AppResult<PostResult> {
        let mut result = PostResult::default();
        let mut folded_text = Vec::new();
        let mut folded_ids = Vec::new();
        for c in &sub.inline {
            let mut position = json!({
                "position_type": "text",
                "base_sha": pr.base_sha,
                "start_sha": pr.start_sha.as_ref().or(pr.base_sha.as_ref()),
                "head_sha": pr.head_sha,
                "old_path": c.old_path,
                "new_path": c.path,
            });
            if let Some(n) = c.new_line {
                position["new_line"] = json!(n);
            }
            if let Some(o) = c.old_line {
                position["old_line"] = json!(o);
            }
            let sent = self
                .req(
                    reqwest::Method::POST,
                    &format!("{}/discussions", Self::mr_path(pr)),
                )
                .json(&json!({ "body": Self::discussion_body(c), "position": position }))
                .send()
                .await;
            let checked = match sent {
                Ok(resp) => check(NAME, resp).await,
                Err(e) => Err(e.into()),
            };
            match checked {
                Ok(resp) => {
                    let note_ref = match resp.json::<Value>().await {
                        Ok(v) => v["notes"][0]["id"]
                            .as_u64()
                            .map(|id| format!("{}#note_{id}", pr.web_url)),
                        Err(e) => {
                            tracing::warn!("unreadable GitLab discussion response: {e}");
                            None
                        }
                    };
                    result.posted.push((c.finding_id.clone(), note_ref));
                }
                // A position GitLab can't resolve: keep the comment, attach it to the summary.
                Err(AppError::Api { status: 400, .. }) => {
                    folded_text.push(fold_comment(c, ""));
                    folded_ids.push(c.finding_id.clone());
                }
                // Discussions already created stay in `result.posted`.
                Err(e) => {
                    result.error = Some(e.to_string());
                    return Ok(result);
                }
            }
        }

        let mut body = sub.body.clone();
        for f in folded_text {
            if !body.is_empty() {
                body.push_str("\n\n");
            }
            body.push_str(&f);
        }
        let finished = if sub.event == ReviewEvent::Approve {
            self.approve(pr, &body).await
        } else if !body.trim().is_empty() {
            self.note(pr, &body).await.map(|note| {
                result.review_url = note["id"]
                    .as_u64()
                    .map(|id| format!("{}#note_{id}", pr.web_url));
            })
        } else {
            Ok(())
        };
        match finished {
            Ok(()) => {
                // Folded comments only exist once the summary note is posted.
                for id in folded_ids {
                    result.posted.push((id.clone(), None));
                    result.folded.push(id);
                }
            }
            Err(e) => result.error = Some(e.to_string()),
        }
        Ok(result)
    }

    async fn approve(&self, pr: &PullRequest, body: &str) -> AppResult<()> {
        // `sha` makes GitLab refuse the approval if new commits arrived since the review.
        let resp = self
            .req(
                reqwest::Method::POST,
                &format!("{}/approve", Self::mr_path(pr)),
            )
            .json(&json!({ "sha": pr.head_sha }))
            .send()
            .await?;
        check(NAME, resp).await?;
        if !body.trim().is_empty() {
            self.note(pr, body).await?;
        }
        Ok(())
    }

    fn git_auth_header(&self) -> String {
        basic_auth_header("oauth2", &self.token)
    }

    fn pr_fetch_ref(&self, number: u64) -> String {
        format!("refs/merge-requests/{number}/head")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{InlineComment, http_client};
    use crate::settings::SettingsOverrides;
    use wiremock::matchers::{body_partial_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn server(base: &str, scope: Option<&str>) -> Server {
        Server {
            id: "srv".into(),
            kind: ProviderKind::Gitlab,
            name: "gl".into(),
            base_url: base.into(),
            username: None,
            pr_scope: scope.map(Into::into),
            settings: SettingsOverrides::default(),
            created_at: String::new(),
        }
    }

    fn mr(iid: u64, author: &str) -> Value {
        json!({
            "iid": iid, "project_id": 42, "title": format!("MR {iid}"), "description": "d",
            "web_url": format!("https://gl/team/app/-/merge_requests/{iid}"), "state": "opened",
            "author": { "username": author }, "sha": "headsha", "source_branch": "feat", "target_branch": "main",
            "draft": false, "updated_at": format!("2026-01-0{iid}T00:00:00Z"),
            "references": { "full": format!("team/app!{iid}") },
            "reviewers": [{ "username": "me" }]
        })
    }

    #[tokio::test]
    async fn lists_group_mrs_with_pagination() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/user"))
            .and(header("private-token", "tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "username": "me" })))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/groups/team%2Fsub/merge_requests"))
            .and(query_param("page", "1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-next-page", "2")
                    .set_body_json(json!([mr(1, "me")])),
            )
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/groups/team%2Fsub/merge_requests"))
            .and(query_param("page", "2"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([mr(2, "bob"), mr(1, "me")])),
            )
            .mount(&mock)
            .await;

        let gl = GitLab::new(
            &server(&mock.uri(), Some("team/sub")),
            "tok".into(),
            None,
            http_client(),
        );
        let prs = gl.list_open_prs(PrFilter::All).await.unwrap();
        assert_eq!(prs.iter().map(|p| p.number).collect::<Vec<_>>(), vec![2, 1]);
        assert_eq!(prs[0].repo, "team/app");
        assert_eq!(prs[0].project_id, "42");
        assert!(prs[0].review_requested && !prs[0].authored_by_me);
        assert!(prs[1].authored_by_me);
    }

    #[tokio::test]
    async fn posts_discussions_and_folds_rejected_positions() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v4/projects/42/merge_requests/7/discussions"))
            .and(body_partial_json(json!({ "position": { "new_path": "a.rs", "new_line": 5, "old_line": 4, "base_sha": "b", "start_sha": "s", "head_sha": "h" } })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "d1", "notes": [{ "id": 99 }] })))
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v4/projects/42/merge_requests/7/discussions"))
            .and(body_partial_json(
                json!({ "position": { "new_path": "b.rs" } }),
            ))
            .respond_with(ResponseTemplate::new(400).set_body_json(
                json!({ "message": "400 Bad request - Note {:line_code=>[\"can't be blank\"]}" }),
            ))
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v4/projects/42/merge_requests/7/notes"))
            .and(body_partial_json(
                json!({ "body": "Summary\n\n**b.rs:9**\n\nOther" }),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 100 })))
            .expect(1)
            .mount(&mock)
            .await;

        let gl = GitLab::new(
            &server(&mock.uri(), None),
            "tok".into(),
            Some("me".into()),
            http_client(),
        );
        let mut pr = gl.map_mr(&mr(7, "bob"), "me").unwrap();
        pr.base_sha = Some("b".into());
        pr.start_sha = Some("s".into());
        pr.head_sha = "h".into();
        let ic = |id: &str, p: &str, line: u32, old: Option<u32>, body: &str| InlineComment {
            finding_id: id.into(),
            path: p.into(),
            old_path: p.into(),
            line,
            start_line: None,
            side: Side::Right,
            new_line: Some(line),
            old_line: old,
            body: body.into(),
            suggestion: None,
        };
        let res = gl
            .post_review(
                &pr,
                &ReviewSubmission {
                    event: ReviewEvent::Comment,
                    body: "Summary".into(),
                    inline: vec![
                        ic("f1", "a.rs", 5, Some(4), "Fix"),
                        ic("f2", "b.rs", 9, None, "Other"),
                    ],
                },
            )
            .await
            .unwrap();
        assert_eq!(res.folded, vec!["f2".to_string()]);
        assert_eq!(
            res.posted[0].1.as_deref(),
            Some("https://gl/team/app/-/merge_requests/7#note_99")
        );
    }

    #[tokio::test]
    async fn approve_pins_head_sha() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v4/projects/42/merge_requests/3/approve"))
            .and(body_partial_json(json!({ "sha": "headsha" })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
            .expect(1)
            .mount(&mock)
            .await;
        let gl = GitLab::new(
            &server(&mock.uri(), None),
            "tok".into(),
            Some("me".into()),
            http_client(),
        );
        let pr = gl.map_mr(&mr(3, "bob"), "me").unwrap();
        gl.approve(&pr, "").await.unwrap();
    }

    #[test]
    fn suggestion_fence_covers_range() {
        let c = InlineComment {
            finding_id: "f".into(),
            path: "a".into(),
            old_path: "a".into(),
            line: 10,
            start_line: Some(8),
            side: Side::Right,
            new_line: Some(10),
            old_line: None,
            body: "b".into(),
            suggestion: Some("x".into()),
        };
        assert_eq!(
            GitLab::discussion_body(&c),
            "b\n\n```suggestion:-2+0\nx\n```"
        );
    }
}
