//! GitLab.com and self-managed GitLab (API v4).

use std::collections::HashSet;
use std::fmt::Write as _;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{
    GitProvider, PostResult, PrFilter, PrReviewInfo, ReviewEvent, ReviewSubmission,
    basic_auth_header, check, fold_comment,
};
use futures_util::StreamExt as _;

use crate::db::models::{
    PrComment, PrComments, PrState, PrThread, ProviderKind, PullRequest, ReviewerState, Server,
    Side,
};
use crate::error::{AppError, AppResult};

const NAME: &str = "GitLab";
const MAX_PAGES: u32 = 10;
/// Merge requests per GraphQL approvals query while listing.
const APPROVAL_BATCH: usize = 25;
/// Approvals queries in flight at once.
const APPROVAL_BATCHES_IN_FLIGHT: usize = 4;

/// Who approved a merge request.
#[derive(Debug, Clone)]
struct Approvals {
    approved_by: Vec<String>,
    /// Enough approvals given. Never true without any approval: GitLab reports a merge request
    /// that needs none as approved.
    approved: bool,
}

impl Approvals {
    fn new(approved_by: Vec<String>, all_given: bool) -> Self {
        Self {
            approved: all_given && !approved_by.is_empty(),
            approved_by,
        }
    }

    /// The merge request's listed reviewers plus its approvers.
    fn status_of(self, pr: &PullRequest, me: &str) -> PrReviewInfo {
        let current = pr.reviewers.iter().map(|r| (r.login.clone(), r.state));
        let approvers = self
            .approved_by
            .into_iter()
            .map(|u| (u, ReviewerState::Approved));
        PrReviewInfo {
            reviewers: super::merge_reviewers(&pr.author, me, current.chain(approvers)),
            approved: self.approved,
        }
    }
}

pub struct GitLab {
    server_id: String,
    api: String,
    graphql: String,
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
        Self {
            server_id: server.id.clone(),
            graphql: format!("{}/api/graphql", api.trim_end_matches("/api/v4")),
            api,
            token,
            username: tokio::sync::OnceCell::new_with(username),
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

    /// A GraphQL query. Partial errors (say, one project the token can't see) still return the
    /// data for the rest, so only a response without data is an error.
    async fn graphql(&self, query: &str, variables: Value) -> AppResult<Value> {
        let resp = self
            .client
            .post(&self.graphql)
            .bearer_auth(&self.token)
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await?;
        let mut v: Value = check(NAME, resp).await?.json().await?;
        match v.get_mut("data").map(Value::take) {
            Some(data) if !data.is_null() => Ok(data),
            _ => Err(AppError::Api {
                provider: NAME,
                status: 200,
                message: v["errors"].to_string(),
            }),
        }
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
    async fn paged(&self, path: &str, query: &[(&str, &str)]) -> AppResult<Vec<Value>> {
        Ok(self.paged_capped(path, query).await?.0)
    }

    /// Like [`Self::paged`], and also tells whether pages were left out at the cap.
    async fn paged_capped(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> AppResult<(Vec<Value>, bool)> {
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let resp = self
                .req(reqwest::Method::GET, path)
                .query(query)
                .query(&[("per_page", "100"), ("page", &page.to_string())])
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
                Some(n) if n > page => return Ok((out, true)),
                _ => break,
            }
        }
        Ok((out, false))
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
            Some("closed" | "locked") => PrState::Closed,
            _ => PrState::Open,
        };
        let diff_refs = &v["diff_refs"];
        let reviewers = super::merge_reviewers(
            &author,
            me,
            v["reviewers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["username"].as_str())
                .map(|u| (u.to_string(), ReviewerState::Requested)),
        );
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
            reviewers,
            // The merge request itself has no approvers; see `review_status`.
            approved: false,
            author,
        })
    }

    /// Approvals of several merge requests in one GraphQL query. `None` for a merge request
    /// GitLab returned nothing for. Repo paths and iids go in as variables, never into the query.
    async fn approvals_batch(&self, prs: &[PullRequest]) -> AppResult<Vec<Option<Approvals>>> {
        let mut params = Vec::with_capacity(prs.len());
        let mut fields = String::new();
        let mut variables = serde_json::Map::new();
        for (i, pr) in prs.iter().enumerate() {
            params.push(format!("$p{i}: ID!, $i{i}: String!"));
            let _ = write!(
                fields,
                " m{i}: project(fullPath: $p{i}) {{ mergeRequest(iid: $i{i}) {{ approved approvedBy {{ nodes {{ username }} }} }} }}"
            );
            variables.insert(format!("p{i}"), json!(pr.repo));
            variables.insert(format!("i{i}"), json!(pr.number.to_string()));
        }
        let query = format!("query({}) {{{fields} }}", params.join(", "));
        let data = self.graphql(&query, Value::Object(variables)).await?;
        Ok((0..prs.len())
            .map(|i| {
                let mr = &data[format!("m{i}").as_str()]["mergeRequest"];
                if mr.is_null() {
                    return None;
                }
                let approved_by = mr["approvedBy"]["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|u| u["username"].as_str().map(str::to_string))
                    .collect();
                Some(Approvals::new(
                    approved_by,
                    mr["approved"].as_bool().unwrap_or(false),
                ))
            })
            .collect())
    }

    /// Adds approvers and approval state to listed merge requests, a batch at a time. A failed
    /// batch only costs those rows their approval state, so it is logged rather than returned.
    async fn add_approvals(&self, prs: &mut [PullRequest], me: &str) {
        // The futures are built by a plain iterator: a closure inside the stream would trip the
        // higher-ranked lifetime check of the `async_trait` future.
        let lookups: Vec<_> = prs
            .chunks(APPROVAL_BATCH)
            .map(|chunk| self.approvals_or_unknown(chunk))
            .collect();
        let batches: Vec<Vec<Option<Approvals>>> = futures_util::stream::iter(lookups)
            .buffered(APPROVAL_BATCHES_IN_FLIGHT)
            .collect()
            .await;
        for (pr, approvals) in prs.iter_mut().zip(batches.into_iter().flatten()) {
            if let Some(a) = approvals {
                a.status_of(pr, me).apply_to(pr);
            }
        }
    }

    async fn approvals_or_unknown(&self, prs: &[PullRequest]) -> Vec<Option<Approvals>> {
        self.approvals_batch(prs).await.unwrap_or_else(|e| {
            tracing::warn!(
                "could not load the approvals of {} merge requests: {e}",
                prs.len()
            );
            vec![None; prs.len()]
        })
    }

    fn map_discussion(d: &Value, me: &str, web_url: &str) -> Option<PrThread> {
        // System notes ("approved this merge request", "added 1 commit") are no comments.
        let first = d["notes"]
            .as_array()?
            .iter()
            .find(|n| n["system"].as_bool() != Some(true))?;
        let comments: Vec<PrComment> = d["notes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|n| n["system"].as_bool() != Some(true))
            .map(|n| {
                let author = n["author"]["username"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let id = n["id"]
                    .as_u64()
                    .map_or_else(String::new, |id| id.to_string());
                PrComment {
                    url: (!id.is_empty() && !web_url.is_empty())
                        .then(|| format!("{web_url}#note_{id}")),
                    id,
                    is_me: !me.is_empty() && author.eq_ignore_ascii_case(me),
                    author,
                    body: n["body"].as_str().unwrap_or_default().into(),
                    created_at: n["created_at"].as_str().unwrap_or_default().into(),
                    review_state: None,
                }
            })
            .collect();
        let position = &first["position"];
        Some(PrThread {
            id: d["id"].as_str().unwrap_or_default().into(),
            file: position["new_path"]
                .as_str()
                .or_else(|| position["old_path"].as_str())
                .map(str::to_string),
            line: position["new_line"]
                .as_u64()
                .or_else(|| position["old_line"].as_u64()),
            resolved: if first["resolvable"].as_bool() == Some(true) {
                first["resolved"].as_bool()
            } else {
                None
            },
            outdated: false,
            comments,
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

    /// The diff position GitLab anchors an inline comment to.
    fn position_json(pr: &PullRequest, c: &super::InlineComment) -> Value {
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
        position
    }

    /// Link to a note of the merge request, if GitLab returned its id.
    fn note_url(pr: &PullRequest, id: &Value) -> Option<String> {
        id.as_u64().map(|id| format!("{}#note_{id}", pr.web_url))
    }

    /// Starts a diff discussion for one inline comment. Returns the link to its note.
    async fn post_discussion(
        &self,
        pr: &PullRequest,
        c: &super::InlineComment,
    ) -> AppResult<Option<String>> {
        let body = json!({
            "body": Self::discussion_body(c),
            "position": Self::position_json(pr, c),
        });
        let resp = self
            .req(
                reqwest::Method::POST,
                &format!("{}/discussions", Self::mr_path(pr)),
            )
            .json(&body)
            .send()
            .await?;
        let resp = check(NAME, resp).await?;
        // The discussion exists either way; only the link is lost.
        Ok(match resp.json::<Value>().await {
            Ok(v) => Self::note_url(pr, &v["notes"][0]["id"]),
            Err(e) => {
                tracing::warn!("unreadable GitLab discussion response: {e}");
                None
            }
        })
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
        const OPENED: (&str, &str) = ("state", "opened");
        let me = self.me().await?;
        let mut raw: Vec<Value> = Vec::new();
        match filter {
            PrFilter::All if !self.groups.is_empty() => {
                for g in &self.groups {
                    let path = format!("/groups/{}/merge_requests", urlencoding::encode(g));
                    raw.extend(self.paged(&path, &[OPENED, ("scope", "all")]).await?);
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
                        &[OPENED, ("scope", "all"), ("reviewer_username", &me)],
                    )
                    .await?;
            }
            PrFilter::Authored => {
                raw = self
                    .paged("/merge_requests", &[OPENED, ("scope", "created_by_me")])
                    .await?;
            }
            PrFilter::Involved => {
                let queries: [&[(&str, &str)]; 3] = [
                    &[OPENED, ("scope", "assigned_to_me")],
                    &[OPENED, ("scope", "all"), ("reviewer_username", &me)],
                    &[OPENED, ("scope", "created_by_me")],
                ];
                for query in queries {
                    raw.extend(self.paged("/merge_requests", query).await?);
                }
            }
        }

        let mut seen = HashSet::new();
        let mut out: Vec<PullRequest> = raw
            .iter()
            .filter_map(|v| self.map_mr(v, &me))
            .filter(|pr| seen.insert((pr.project_id.clone(), pr.number)))
            .collect();
        // Merge request lists carry no approvers.
        self.add_approvals(&mut out, &me).await;
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
            match self.post_discussion(pr, c).await {
                Ok(note_ref) => result.posted.push((c.finding_id.clone(), note_ref)),
                // A position GitLab can't resolve: keep the comment, attach it to the summary.
                Err(AppError::Api { status: 400, .. }) => {
                    folded_text.push(fold_comment(c));
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
            self.approve(pr, &body).await.map(|()| None)
        } else if !body.trim().is_empty() {
            self.note(pr, &body)
                .await
                .map(|note| Self::note_url(pr, &note["id"]))
        } else {
            Ok(None)
        };
        match finished {
            Ok(review_url) => {
                result.review_url = review_url;
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

    async fn review_status(&self, pr: &PullRequest) -> AppResult<PrReviewInfo> {
        let v = self
            .get_json(&format!("{}/approvals", Self::mr_path(pr)))
            .await?;
        let approved_by = v["approved_by"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a["user"]["username"].as_str().map(str::to_string))
            .collect();
        let all_given = v["approvals_left"].as_u64().unwrap_or(0) == 0;
        // Only decides which entry is the user's.
        let me = self.me().await.unwrap_or_default();
        Ok(Approvals::new(approved_by, all_given).status_of(pr, &me))
    }

    async fn list_comments(&self, project_id: &str, number: u64) -> AppResult<PrComments> {
        let mr_path = format!("/projects/{project_id}/merge_requests/{number}");
        let discussions_path = format!("{mr_path}/discussions");
        // Notes carry no link, so they are linked from the merge request's own URL.
        let (mr, (discussions, truncated)) = tokio::try_join!(
            self.get_json(&mr_path),
            self.paged_capped(&discussions_path, &[]),
        )?;
        let web_url = mr["web_url"].as_str().unwrap_or_default();
        let me = self.me().await.unwrap_or_else(|e| {
            tracing::warn!("could not load the GitLab username: {e}");
            String::new()
        });
        let mut threads: Vec<PrThread> = discussions
            .iter()
            .filter_map(|d| Self::map_discussion(d, &me, web_url))
            .collect();
        threads.sort_by(|a, b| a.comments[0].created_at.cmp(&b.comments[0].created_at));
        Ok(PrComments { threads, truncated })
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
        // One query for both: MR 2 is approved by carol, MR 1 came back empty, which must not
        // fail the list.
        Mock::given(method("POST"))
            .and(path("/api/graphql"))
            .and(header("authorization", "Bearer tok"))
            .and(body_partial_json(json!({ "variables": { "p0": "team/app", "i0": "1", "p1": "team/app", "i1": "2" } })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "m0": null,
                    "m1": { "mergeRequest": { "approved": true, "approvedBy": { "nodes": [{ "username": "carol" }, { "username": "me" }] } } }
                },
                "errors": [{ "message": "m0 not accessible" }]
            })))
            .expect(1)
            .mount(&mock)
            .await;
        let prs = gl.list_open_prs(PrFilter::All).await.unwrap();
        assert_eq!(prs.iter().map(|p| p.number).collect::<Vec<_>>(), vec![2, 1]);
        assert!(prs[0].approved && !prs[1].approved);
        assert_eq!(
            prs[0]
                .reviewers
                .iter()
                .map(|r| (r.login.as_str(), r.state, r.is_me))
                .collect::<Vec<_>>(),
            vec![
                ("me", ReviewerState::Approved, true),
                ("carol", ReviewerState::Approved, false),
            ]
        );
        // The author "me" is no reviewer of their own MR.
        assert!(prs[1].reviewers.is_empty());
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
    fn approved_needs_at_least_one_approval() {
        let pr = GitLab::new(&server("https://gl", None), "t".into(), None, http_client())
            .map_mr(&mr(3, "bob"), "me")
            .unwrap();
        // GitLab says approved when nothing is required, even with no approvals.
        assert!(!Approvals::new(vec![], true).status_of(&pr, "me").approved);
        assert!(
            !Approvals::new(vec!["x".into()], false)
                .status_of(&pr, "me")
                .approved
        );
        let status = Approvals::new(vec!["x".into()], true).status_of(&pr, "me");
        assert!(status.approved);
        assert_eq!(
            status
                .reviewers
                .iter()
                .map(|r| (r.login.as_str(), r.state))
                .collect::<Vec<_>>(),
            vec![
                ("me", ReviewerState::Requested),
                ("x", ReviewerState::Approved)
            ]
        );
    }

    #[tokio::test]
    async fn review_status_reads_the_approvals_endpoint() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/42/merge_requests/3/approvals"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "approved": false, "approvals_left": 1, "approved_by": [{ "user": { "username": "x" } }]
            })))
            .mount(&mock)
            .await;
        let gl = GitLab::new(
            &server(&mock.uri(), None),
            "tok".into(),
            Some("me".into()),
            http_client(),
        );
        let pr = gl.map_mr(&mr(3, "bob"), "me").unwrap();
        let status = gl.review_status(&pr).await.unwrap();
        assert!(!status.approved, "one approval still missing");
        assert_eq!(status.reviewers.len(), 2);
    }

    #[tokio::test]
    async fn approvals_batch_fails_soft_without_data() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": null, "errors": [{ "message": "Field 'approved' doesn't exist" }]
            })))
            .mount(&mock)
            .await;
        let gl = GitLab::new(
            &server(&mock.uri(), None),
            "tok".into(),
            Some("me".into()),
            http_client(),
        );
        let mut prs = vec![gl.map_mr(&mr(3, "bob"), "me").unwrap()];
        gl.add_approvals(&mut prs, "me").await;
        assert!(!prs[0].approved);
        assert_eq!(prs[0].reviewers.len(), 1);
    }

    #[tokio::test]
    async fn lists_discussions_without_system_notes() {
        let mock = MockServer::start().await;
        let note = |id: u64, who: &str, at: &str, system: bool| {
            json!({ "id": id, "body": format!("note {id}"), "author": { "username": who },
                    "created_at": at, "system": system, "resolvable": false })
        };
        let mut inline = note(3, "bob", "2026-01-01", false);
        inline["resolvable"] = json!(true);
        inline["resolved"] = json!(false);
        inline["position"] =
            json!({ "new_path": null, "old_path": "gone.rs", "new_line": null, "old_line": 4 });
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/42/merge_requests/7"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "web_url": "https://gl/mr/7" })),
            )
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/42/merge_requests/7/discussions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-next-page", "11")
                    .set_body_json(json!([
                        { "id": "d1", "notes": [note(1, "me", "2026-01-02", false)] },
                        { "id": "d2", "notes": [note(2, "bob", "2026-01-01", true)] },
                        { "id": "d3", "notes": [inline, note(4, "me", "2026-01-03", false)] }
                    ])),
            )
            .mount(&mock)
            .await;
        let gl = GitLab::new(
            &server(&mock.uri(), None),
            "tok".into(),
            Some("me".into()),
            http_client(),
        );
        let out = gl.list_comments("42", 7).await.unwrap();
        assert!(out.truncated, "the next page is past the cap");
        assert_eq!(
            out.threads
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["d3", "d1"]
        );
        let t = &out.threads[0];
        assert_eq!(
            (t.file.as_deref(), t.line, t.resolved),
            (Some("gone.rs"), Some(4), Some(false))
        );
        assert_eq!(t.comments.len(), 2);
        assert!(t.comments[1].is_me && !t.comments[0].is_me);
        assert_eq!(t.comments[0].url.as_deref(), Some("https://gl/mr/7#note_3"));
        assert!(out.threads[1].resolved.is_none() && out.threads[1].file.is_none());
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
