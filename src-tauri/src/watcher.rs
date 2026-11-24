//! Background polling of reviewed PRs: new-change alerts and auto re-reviews.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::db::models::{PrState, PullRequest, ReviewTrigger, TrackedPr, Verdict};
use crate::error::AppError;
use crate::review::queue;
use crate::service::Service;

const TICK: Duration = Duration::from_secs(30);
const ERROR_BACKOFF: Duration = Duration::from_secs(300);

/// The head commit to remember for a tracked PR.
#[derive(Debug, PartialEq, Eq)]
pub struct HeadUpdate {
    pub sha: String,
    /// The new head has not been reviewed yet.
    pub has_new_changes: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub deactivate: bool,
    pub update_head: Option<HeadUpdate>,
    pub notify_new_changes: bool,
    pub auto_review: bool,
}

/// Pure decision logic, kept separate from I/O for testing.
pub fn decide(
    tracked: &TrackedPr,
    state: PrState,
    head_sha: &str,
    auto_review_enabled: bool,
) -> Decision {
    if state != PrState::Open {
        return Decision {
            deactivate: true,
            ..Default::default()
        };
    }
    if tracked.last_seen_head_sha.as_deref() == Some(head_sha) {
        return Decision::default();
    }
    let unreviewed = tracked.last_reviewed_sha.as_deref() != Some(head_sha);
    Decision {
        deactivate: false,
        update_head: Some(HeadUpdate {
            sha: head_sha.to_string(),
            has_new_changes: unreviewed,
        }),
        notify_new_changes: unreviewed,
        auto_review: unreviewed
            && auto_review_enabled
            && tracked.last_verdict != Some(Verdict::Approve),
    }
}

pub fn spawn(svc: Arc<Service>) {
    tauri::async_runtime::spawn(async move {
        let mut last_checked: HashMap<(String, String, u64), Instant> = HashMap::new();
        let mut server_backoff: HashMap<String, Instant> = HashMap::new();
        loop {
            tick(&svc, &mut last_checked, &mut server_backoff).await;
            tokio::time::sleep(TICK).await;
        }
    });
}

async fn tick(
    svc: &Arc<Service>,
    last_checked: &mut HashMap<(String, String, u64), Instant>,
    server_backoff: &mut HashMap<String, Instant>,
) {
    let tracked = match svc.db.list_tracked() {
        Ok(tracked) => tracked,
        Err(e) => {
            tracing::warn!("could not list tracked PRs: {e}");
            return;
        }
    };
    let now = Instant::now();
    let mut changed = false;

    for t in tracked {
        if server_backoff
            .get(&t.server_id)
            .is_some_and(|until| *until > now)
        {
            continue;
        }
        let settings = match svc.effective_settings(&t.server_id, Some(&t.repo)) {
            Ok(settings) => settings,
            Err(e) => {
                tracing::warn!("skipping {}#{}: {e}", t.repo, t.number);
                continue;
            }
        };
        let key = (t.server_id.clone(), t.repo.clone(), t.number);
        let interval = Duration::from_secs(u64::from(settings.poll_interval_secs.value));
        if last_checked
            .get(&key)
            .is_some_and(|at| now.duration_since(*at) < interval)
        {
            continue;
        }
        last_checked.insert(key, now);

        match poll_one(svc, &t, settings.auto_review_on_new_commits.value).await {
            Polled::Unchanged => {}
            Polled::Changed => changed = true,
            Polled::Failed => {
                server_backoff.insert(t.server_id.clone(), now + ERROR_BACKOFF);
            }
        }
    }
    if changed {
        svc.sink.tracked_updated();
    }
}

/// What polling one tracked PR did.
enum Polled {
    Unchanged,
    /// The tracked row changed, so the UI needs a refresh.
    Changed,
    /// The server could not be reached; back off before polling it again.
    Failed,
}

/// Checks one tracked PR for new commits and acts on [`decide`]'s decision.
async fn poll_one(svc: &Arc<Service>, t: &TrackedPr, auto_review_enabled: bool) -> Polled {
    let provider = match svc.provider_for(&t.server_id) {
        Ok((_server, provider)) => provider,
        Err(e) => {
            tracing::warn!("skipping {}#{}: {e}", t.repo, t.number);
            return Polled::Unchanged;
        }
    };
    let fresh: PullRequest = match provider.get_pr(&t.project_id, t.number).await {
        Ok(pr) => pr,
        Err(AppError::Api { status: 404, .. }) => {
            deactivate(svc, t);
            return Polled::Changed;
        }
        Err(e) => {
            tracing::warn!("polling {}#{} failed: {e}", t.repo, t.number);
            return Polled::Failed;
        }
    };

    // A review still running for an older head is superseded by the queue.
    let d = decide(t, fresh.state, &fresh.head_sha, auto_review_enabled);
    if d.deactivate {
        deactivate(svc, t);
        return Polled::Changed;
    }
    let title = format!("New changes on {}#{}", t.repo, t.number);
    if d.auto_review {
        let started = format!("\"{}\" changed. An auto-review has started.", fresh.title);
        if let Err(e) = queue::enqueue(svc, fresh, ReviewTrigger::Auto) {
            // Leave the stored head alone so the next poll tries again.
            tracing::warn!(
                "auto-review of {}#{} failed to queue: {e}",
                t.repo,
                t.number
            );
            return Polled::Unchanged;
        }
        svc.sink.notify(&title, &started);
    } else if d.notify_new_changes {
        svc.sink.notify(
            &title,
            &format!(
                "\"{}\" has new commits since your last review.",
                fresh.title
            ),
        );
    }
    let Some(head) = d.update_head else {
        return Polled::Unchanged;
    };
    if let Err(e) = svc.db.set_tracked_head(
        &t.server_id,
        &t.repo,
        t.number,
        &head.sha,
        head.has_new_changes,
    ) {
        tracing::warn!("updating {}#{} failed: {e}", t.repo, t.number);
    }
    Polled::Changed
}

fn deactivate(svc: &Service, t: &TrackedPr) {
    if let Err(e) = svc
        .db
        .set_tracked_active(&t.server_id, &t.repo, t.number, false)
    {
        tracing::warn!("deactivating {}#{} failed: {e}", t.repo, t.number);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked(seen: &str, reviewed: Option<&str>, verdict: Option<Verdict>) -> TrackedPr {
        TrackedPr {
            server_id: "s".into(),
            repo: "r".into(),
            project_id: "p".into(),
            number: 1,
            title: "t".into(),
            author: "a".into(),
            web_url: String::new(),
            last_seen_head_sha: Some(seen.into()),
            last_reviewed_sha: reviewed.map(Into::into),
            last_verdict: verdict,
            has_new_changes: false,
            active: true,
            updated_at: String::new(),
        }
    }

    #[test]
    fn no_change_means_no_action() {
        let t = tracked("a", Some("a"), Some(Verdict::RequestChanges));
        assert_eq!(decide(&t, PrState::Open, "a", true), Decision::default());
    }

    #[test]
    fn new_commits_alert_and_auto_review_when_not_approved() {
        let t = tracked("a", Some("a"), Some(Verdict::RequestChanges));
        let d = decide(&t, PrState::Open, "b", true);
        assert_eq!(
            d.update_head,
            Some(HeadUpdate {
                sha: "b".into(),
                has_new_changes: true
            })
        );
        assert!(d.notify_new_changes && d.auto_review);

        let d = decide(&t, PrState::Open, "b", false);
        assert!(d.notify_new_changes && !d.auto_review);
    }

    #[test]
    fn no_auto_review_after_approve() {
        let approved = tracked("a", Some("a"), Some(Verdict::Approve));
        let d = decide(&approved, PrState::Open, "b", true);
        assert!(d.notify_new_changes && !d.auto_review);
    }

    #[test]
    fn new_commits_during_a_running_review_still_auto_review() {
        // The queue supersedes the running review of the older head.
        let t = tracked("a", None, None);
        let d = decide(&t, PrState::Open, "b", true);
        assert!(d.auto_review);
    }

    #[test]
    fn closed_or_merged_prs_are_deactivated() {
        let t = tracked("a", Some("a"), None);
        assert!(decide(&t, PrState::Merged, "a", true).deactivate);
        assert!(decide(&t, PrState::Closed, "z", true).deactivate);
    }

    #[test]
    fn head_matching_last_review_is_not_new() {
        // E.g. a force-push back to the reviewed commit.
        let t = tracked("x", Some("a"), Some(Verdict::Comment));
        let d = decide(&t, PrState::Open, "a", true);
        assert_eq!(
            d.update_head,
            Some(HeadUpdate {
                sha: "a".into(),
                has_new_changes: false
            })
        );
        assert!(!d.notify_new_changes && !d.auto_review);
    }
}
