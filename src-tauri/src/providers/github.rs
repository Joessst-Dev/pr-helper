//! GitHub.com and GitHub Enterprise Server.

use std::fmt::Write as _;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{
    GitProvider, PostResult, PrFilter, ReviewEvent, ReviewSubmission, basic_auth_header, check,
    fold_comment,
};
use crate::db::models::{PrState, ProviderKind, PullRequest, Server, Side};
use crate::error::{AppError, AppResult};

const NAME: &str = "GitHub";
const MAX_SEARCH_PAGES: usize = 10;

pub struct GitHub {
    server_id: String,
    api: String,
    graphql: String,
    token: String,
    username: tokio::sync::OnceCell<String>,
    scope: Option<String>,
    client: reqwest::Client,
}

/// Accepts `https://github.com`, `https://api.github.com`, a GHE root or a GHE `/api/v3` URL.
pub fn api_urls(base: &str) -> (String, String) {
    let base = base.trim().trim_end_matches('/');
    if base == "https://github.com"
        || base == "https://api.github.com"
        || base == "https://www.github.com"
    {
        return (
            "https://api.github.com".into(),
            "https://api.github.com/graphql".into(),
        );
    }
    let api = if base.ends_with("/api/v3") {
        base.to_string()
    } else {
        format!("{base}/api/v3")
    };
    let graphql = format!("{}/api/graphql", api.trim_end_matches("/api/v3"));
    (api, graphql)
}

impl GitHub {
    pub fn new(
        server: &Server,
        token: String,
        username: Option<String>,
        client: reqwest::Client,
    ) -> Self {
        let (api, graphql) = api_urls(&server.base_url);
        let cell = tokio::sync::OnceCell::new();
        if let Some(u) = username {
            let _ = cell.set(u);
        }
        Self {
            server_id: server.id.clone(),
            api,
            graphql,
            token,
            username: cell,
            scope: server.pr_scope.clone().filter(|s| !s.trim().is_empty()),
            client,
        }
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.api))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    async fn graphql(&self, query: &str, variables: Value) -> AppResult<Value> {
        let resp = self
            .client
            .post(&self.graphql)
            .bearer_auth(&self.token)
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await?;
        let v: Value = check(NAME, resp).await?.json().await?;
        if let Some(errors) = v
            .get("errors")
            .filter(|e| e.as_array().is_some_and(|a| !a.is_empty()))
        {
            return Err(AppError::Api {
                provider: NAME,
                status: 200,
                message: errors.to_string(),
            });
        }
        Ok(v["data"].clone())
    }

    async fn login(&self) -> AppResult<String> {
        self.username
            .get_or_try_init(|| async {
                let resp = self.req(reqwest::Method::GET, "/user").send().await?;
                let v: Value = check(NAME, resp).await?.json().await?;
                v["login"]
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| AppError::invalid("GitHub /user returned no login"))
            })
            .await
            .cloned()
    }

    async fn orgs(&self) -> AppResult<Vec<String>> {
        let data = self
            .graphql(
                "query { viewer { organizations(first: 100) { nodes { login } } } }",
                json!({}),
            )
            .await?;
        Ok(data["viewer"]["organizations"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|n| n["login"].as_str().map(str::to_string))
            .collect())
    }

    async fn search_query(&self, filter: PrFilter) -> AppResult<String> {
        let mut q = String::from("is:pr is:open archived:false");
        match filter {
            PrFilter::ReviewRequested => q.push_str(" review-requested:@me"),
            PrFilter::Authored => q.push_str(" author:@me"),
            PrFilter::Involved => q.push_str(" involves:@me"),
            PrFilter::All => {}
        }
        match (&self.scope, filter) {
            (Some(scope), _) => {
                q.push(' ');
                q.push_str(scope.trim());
            }
            (None, PrFilter::All) => {
                // Multiple user:/org: qualifiers are ORed by GitHub search.
                let _ = write!(q, " user:{}", self.login().await?);
                for org in self.orgs().await? {
                    let _ = write!(q, " org:{org}");
                }
            }
            (None, _) => {}
        }
        Ok(q)
    }

    fn map_search_node(&self, n: &Value, me: &str) -> Option<PullRequest> {
        let repo = n["repository"]["nameWithOwner"].as_str()?.to_string();
        let author = n["author"]["login"].as_str().unwrap_or("ghost").to_string();
        let review_requested = n["reviewRequests"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|r| r["requestedReviewer"]["login"].as_str() == Some(me));
        Some(PullRequest {
            server_id: self.server_id.clone(),
            project_id: repo.clone(),
            number: n["number"].as_u64()?,
            title: n["title"].as_str().unwrap_or_default().into(),
            description: n["body"].as_str().unwrap_or_default().into(),
            web_url: n["url"].as_str().unwrap_or_default().into(),
            clone_url: n["repository"]["url"].as_str().map(|u| format!("{u}.git")),
            head_sha: n["headRefOid"].as_str().unwrap_or_default().into(),
            base_sha: n["baseRefOid"].as_str().map(str::to_string),
            start_sha: None,
            head_ref: n["headRefName"].as_str().unwrap_or_default().into(),
            base_ref: n["baseRefName"].as_str().unwrap_or_default().into(),
            draft: n["isDraft"].as_bool().unwrap_or(false),
            state: PrState::Open,
            updated_at: n["updatedAt"].as_str().unwrap_or_default().into(),
            review_requested,
            authored_by_me: author == me,
            author,
            repo,
        })
    }

    fn map_rest_pr(&self, v: &Value, me: &str) -> AppResult<PullRequest> {
        let repo = v["base"]["repo"]["full_name"]
            .as_str()
            .ok_or_else(|| AppError::invalid("pull request has no base repository"))?
            .to_string();
        let author = v["user"]["login"].as_str().unwrap_or("ghost").to_string();
        let state = if v["merged"].as_bool().unwrap_or(false) || !v["merged_at"].is_null() {
            PrState::Merged
        } else if v["state"] == "closed" {
            PrState::Closed
        } else {
            PrState::Open
        };
        Ok(PullRequest {
            server_id: self.server_id.clone(),
            project_id: repo.clone(),
            number: v["number"].as_u64().unwrap_or_default(),
            title: v["title"].as_str().unwrap_or_default().into(),
            description: v["body"].as_str().unwrap_or_default().into(),
            web_url: v["html_url"].as_str().unwrap_or_default().into(),
            clone_url: v["base"]["repo"]["clone_url"].as_str().map(str::to_string),
            head_sha: v["head"]["sha"].as_str().unwrap_or_default().into(),
            base_sha: v["base"]["sha"].as_str().map(str::to_string),
            start_sha: None,
            head_ref: v["head"]["ref"].as_str().unwrap_or_default().into(),
            base_ref: v["base"]["ref"].as_str().unwrap_or_default().into(),
            draft: v["draft"].as_bool().unwrap_or(false),
            state,
            updated_at: v["updated_at"].as_str().unwrap_or_default().into(),
            review_requested: v["requested_reviewers"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|r| r["login"].as_str() == Some(me)),
            authored_by_me: author == me,
            author,
            repo,
        })
    }

    fn comment_json(c: &super::InlineComment) -> Value {
        let mut body = c.body.clone();
        if let (Some(s), Side::Right) = (&c.suggestion, c.side) {
            let fence = super::fence_for(s);
            let _ = write!(body, "\n\n{fence}suggestion\n{s}\n{fence}");
        }
        let side = if c.side == Side::Left {
            "LEFT"
        } else {
            "RIGHT"
        };
        let mut v = json!({ "path": c.path, "line": c.line, "side": side, "body": body });
        if let Some(start) = c.start_line.filter(|s| *s < c.line) {
            v["start_line"] = json!(start);
            v["start_side"] = json!(side);
        }
        v
    }

    async fn submit(
        &self,
        pr: &PullRequest,
        event: &str,
        body: &str,
        comments: Vec<Value>,
    ) -> AppResult<Value> {
        let mut payload = json!({ "commit_id": pr.head_sha, "event": event, "comments": comments });
        if !body.is_empty() {
            payload["body"] = json!(body);
        }
        let resp = self
            .req(
                reqwest::Method::POST,
                &format!("/repos/{}/pulls/{}/reviews", pr.project_id, pr.number),
            )
            .json(&payload)
            .send()
            .await?;
        Ok(check(NAME, resp).await?.json().await?)
    }
}

#[async_trait]
impl GitProvider for GitHub {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Github
    }

    async fn current_user(&self) -> AppResult<String> {
        self.login().await
    }

    async fn list_open_prs(&self, filter: PrFilter) -> AppResult<Vec<PullRequest>> {
        const QUERY: &str = r"
query($q: String!, $cursor: String) {
  viewer { login }
  search(query: $q, type: ISSUE, first: 50, after: $cursor) {
    pageInfo { hasNextPage endCursor }
    nodes {
      ... on PullRequest {
        number title body url isDraft updatedAt headRefOid baseRefOid headRefName baseRefName
        author { login }
        repository { nameWithOwner url }
        reviewRequests(first: 20) { nodes { requestedReviewer { ... on User { login } } } }
      }
    }
  }
}";
        let q = self.search_query(filter).await?;
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_SEARCH_PAGES {
            let data = self
                .graphql(QUERY, json!({ "q": q, "cursor": cursor }))
                .await?;
            let me = data["viewer"]["login"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            for n in data["search"]["nodes"].as_array().into_iter().flatten() {
                if let Some(pr) = self.map_search_node(n, &me) {
                    out.push(pr);
                }
            }
            let page = &data["search"]["pageInfo"];
            if page["hasNextPage"].as_bool() != Some(true) {
                break;
            }
            cursor = page["endCursor"].as_str().map(str::to_string);
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    async fn get_pr(&self, project_id: &str, number: u64) -> AppResult<PullRequest> {
        let resp = self
            .req(
                reqwest::Method::GET,
                &format!("/repos/{project_id}/pulls/{number}"),
            )
            .send()
            .await?;
        let v: Value = check(NAME, resp).await?.json().await?;
        // The login only decides the "requested from me"/"authored by me" flags, which are not
        // worth failing the fetch for.
        let me = self.login().await.unwrap_or_else(|e| {
            tracing::warn!("could not load the GitHub login: {e}");
            String::new()
        });
        self.map_rest_pr(&v, &me)
    }

    async fn post_review(&self, pr: &PullRequest, sub: &ReviewSubmission) -> AppResult<PostResult> {
        let event = match sub.event {
            ReviewEvent::Comment => "COMMENT",
            ReviewEvent::RequestChanges => "REQUEST_CHANGES",
            ReviewEvent::Approve => "APPROVE",
        };
        let comments: Vec<Value> = sub.inline.iter().map(Self::comment_json).collect();
        let first = self.submit(pr, event, &sub.body, comments).await;

        let (resp, folded) = match first {
            Ok(v) => (v, Vec::new()),
            // 422 usually means a line is outside the diff. Reviews are atomic, so fold every
            // inline comment into the body rather than losing the whole review.
            Err(AppError::Api {
                status: 422,
                message,
                ..
            }) if !sub.inline.is_empty()
                && !message.to_lowercase().contains("own pull request") =>
            {
                let mut body = sub.body.clone();
                for c in &sub.inline {
                    if !body.is_empty() {
                        body.push_str("\n\n");
                    }
                    body.push_str(&fold_comment(c, ""));
                }
                let v = self.submit(pr, event, &body, Vec::new()).await?;
                (v, sub.inline.iter().map(|c| c.finding_id.clone()).collect())
            }
            Err(e) => return Err(e),
        };
        let review_url = resp["html_url"].as_str().map(str::to_string);
        Ok(PostResult {
            posted: sub
                .inline
                .iter()
                .map(|c| (c.finding_id.clone(), review_url.clone()))
                .collect(),
            folded,
            review_url,
            error: None,
        })
    }

    async fn approve(&self, pr: &PullRequest, body: &str) -> AppResult<()> {
        self.submit(pr, "APPROVE", body, Vec::new()).await?;
        Ok(())
    }

    fn git_auth_header(&self) -> String {
        basic_auth_header("x-access-token", &self.token)
    }

    fn pr_fetch_ref(&self, number: u64) -> String {
        format!("refs/pull/{number}/head")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{InlineComment, http_client};
    use crate::settings::SettingsOverrides;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn server(base: &str, scope: Option<&str>) -> Server {
        Server {
            id: "srv".into(),
            kind: ProviderKind::Github,
            name: "gh".into(),
            base_url: base.into(),
            username: None,
            pr_scope: scope.map(Into::into),
            settings: SettingsOverrides::default(),
            created_at: String::new(),
        }
    }

    #[test]
    fn api_url_variants() {
        assert_eq!(api_urls("https://github.com/").0, "https://api.github.com");
        assert_eq!(
            api_urls("https://ghe.corp"),
            (
                "https://ghe.corp/api/v3".into(),
                "https://ghe.corp/api/graphql".into()
            )
        );
        assert_eq!(
            api_urls("https://ghe.corp/api/v3").1,
            "https://ghe.corp/api/graphql"
        );
    }

    fn pr_fixture() -> Value {
        json!({
            "number": 12, "title": "Fix", "body": "desc", "html_url": "https://gh/x/y/pull/12",
            "state": "open", "merged": false, "merged_at": null, "draft": false,
            "updated_at": "2026-01-01T00:00:00Z",
            "user": { "login": "alice" },
            "head": { "sha": "headsha", "ref": "feat" },
            "base": { "sha": "basesha", "ref": "main", "repo": { "full_name": "x/y", "clone_url": "https://gh/x/y.git" } },
            "requested_reviewers": [{ "login": "me" }]
        })
    }

    #[tokio::test]
    async fn lists_prs_with_scope_and_maps_fields() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/graphql"))
            .and(header("authorization", "Bearer tok"))
            .and(body_partial_json(json!({
                "variables": { "q": "is:pr is:open archived:false review-requested:@me org:acme" }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                "viewer": { "login": "me" },
                "search": {
                    "pageInfo": { "hasNextPage": false, "endCursor": null },
                    "nodes": [{
                        "number": 5, "title": "T", "body": "", "url": "https://gh/acme/app/pull/5",
                        "isDraft": true, "updatedAt": "2026-02-01T00:00:00Z", "headRefOid": "h", "baseRefOid": "b",
                        "headRefName": "f", "baseRefName": "main", "author": { "login": "me" },
                        "repository": { "nameWithOwner": "acme/app", "url": "https://gh/acme/app" },
                        "reviewRequests": { "nodes": [{ "requestedReviewer": { "login": "me" } }] }
                    }, {}]
                }
            }})))
            .mount(&mock)
            .await;

        let gh = GitHub::new(
            &server(&format!("{}/api/v3", mock.uri()), Some("org:acme")),
            "tok".into(),
            None,
            http_client(),
        );
        let prs = gh.list_open_prs(PrFilter::ReviewRequested).await.unwrap();
        assert_eq!(prs.len(), 1);
        let pr = &prs[0];
        assert_eq!(pr.repo, "acme/app");
        assert_eq!(pr.clone_url.as_deref(), Some("https://gh/acme/app.git"));
        assert!(pr.draft && pr.review_requested && pr.authored_by_me);
    }

    #[tokio::test]
    async fn gets_pr_and_posts_review_with_inline_comments() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "login": "me" })))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/repos/x/y/pulls/12"))
            .respond_with(ResponseTemplate::new(200).set_body_json(pr_fixture()))
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v3/repos/x/y/pulls/12/reviews"))
            .and(body_partial_json(json!({
                "commit_id": "headsha", "event": "REQUEST_CHANGES", "body": "Summary",
                "comments": [{ "path": "src/a.rs", "line": 9, "start_line": 7, "start_side": "RIGHT", "side": "RIGHT",
                               "body": "Fix it\n\n```suggestion\nlet x = 1;\n```" }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 1, "html_url": "https://gh/r/1" })))
            .expect(1)
            .mount(&mock)
            .await;

        let gh = GitHub::new(
            &server(&format!("{}/api/v3", mock.uri()), None),
            "tok".into(),
            None,
            http_client(),
        );
        let pr = gh.get_pr("x/y", 12).await.unwrap();
        assert_eq!(pr.head_sha, "headsha");
        assert_eq!(pr.base_sha.as_deref(), Some("basesha"));
        assert!(pr.review_requested && !pr.authored_by_me);
        assert_eq!(pr.state, PrState::Open);

        let res = gh
            .post_review(
                &pr,
                &ReviewSubmission {
                    event: ReviewEvent::RequestChanges,
                    body: "Summary".into(),
                    inline: vec![InlineComment {
                        finding_id: "f1".into(),
                        path: "src/a.rs".into(),
                        old_path: "src/a.rs".into(),
                        line: 9,
                        start_line: Some(7),
                        side: Side::Right,
                        new_line: Some(9),
                        old_line: None,
                        body: "Fix it".into(),
                        suggestion: Some("let x = 1;".into()),
                    }],
                },
            )
            .await
            .unwrap();
        assert_eq!(res.review_url.as_deref(), Some("https://gh/r/1"));
        assert!(res.folded.is_empty());
    }

    #[tokio::test]
    async fn folds_inline_comments_when_github_rejects_lines() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v3/repos/x/y/pulls/12/reviews"))
            .and(body_partial_json(
                json!({ "comments": [{ "path": "a.rs" }] }),
            ))
            .respond_with(ResponseTemplate::new(422).set_body_json(json!({
                "message": "Unprocessable Entity", "errors": ["Line could not be resolved"]
            })))
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v3/repos/x/y/pulls/12/reviews"))
            .and(body_partial_json(
                json!({ "comments": [], "body": "Sum\n\n**a.rs:3**\n\nNope" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "html_url": "u" })))
            .expect(1)
            .mount(&mock)
            .await;

        let gh = GitHub::new(
            &server(&format!("{}/api/v3", mock.uri()), None),
            "t".into(),
            Some("me".into()),
            http_client(),
        );
        let pr = gh.map_rest_pr(&pr_fixture(), "me").unwrap();
        let res = gh
            .post_review(
                &pr,
                &ReviewSubmission {
                    event: ReviewEvent::Comment,
                    body: "Sum".into(),
                    inline: vec![InlineComment {
                        finding_id: "f1".into(),
                        path: "a.rs".into(),
                        old_path: "a.rs".into(),
                        line: 3,
                        start_line: None,
                        side: Side::Right,
                        new_line: Some(3),
                        old_line: None,
                        body: "Nope".into(),
                        suggestion: None,
                    }],
                },
            )
            .await
            .unwrap();
        assert_eq!(res.folded, vec!["f1".to_string()]);
    }

    #[test]
    fn merged_state_and_auth_header() {
        let gh = GitHub::new(
            &server("https://github.com", None),
            "tok".into(),
            None,
            http_client(),
        );
        let mut v = pr_fixture();
        v["merged"] = json!(true);
        assert_eq!(gh.map_rest_pr(&v, "me").unwrap().state, PrState::Merged);
        assert!(gh.git_auth_header().starts_with("Authorization: Basic "));
        assert_eq!(gh.pr_fetch_ref(3), "refs/pull/3/head");
    }
}
