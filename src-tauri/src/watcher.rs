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

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub deactivate: bool,
    /// (new head, has_new_changes)
    pub update_head: Option<(String, bool)>,
    pub notify_new_changes: bool,
    pub auto_review: bool,
}

/// Pure decision logic, kept separate from I/O for testing.
pub fn decide(
    tracked: &TrackedPr,
    state: PrState,
    head_sha: &str,
    auto_review_enabled: bool,
    review_active: bool,
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
        update_head: Some((head_sha.to_string(), unreviewed)),
        notify_new_changes: unreviewed,
        auto_review: unreviewed
            && auto_review_enabled
            && !review_active
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
    let Ok(tracked) = svc.db.list_tracked(true) else {
        return;
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
        let Ok(settings) = svc.effective_settings(&t.server_id, Some(&t.repo)) else {
            continue;
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

        let Ok((_server, provider)) = svc.provider_for(&t.server_id) else {
            continue;
        };
        let fresh: PullRequest = match provider.get_pr(&t.project_id, t.number).await {
            Ok(pr) => pr,
            Err(AppError::Api { status: 404, .. }) => {
                let _ = svc
                    .db
                    .set_tracked_active(&t.server_id, &t.repo, t.number, false);
                changed = true;
                continue;
            }
            Err(e) => {
                tracing::warn!("polling {}#{} failed: {e}", t.repo, t.number);
                server_backoff.insert(t.server_id.clone(), now + ERROR_BACKOFF);
                continue;
            }
        };

        let active = svc.queue.is_active(&t.server_id, &t.repo, t.number);
        let d = decide(
            &t,
            fresh.state,
            &fresh.head_sha,
            settings.auto_review_on_new_commits.value,
            active,
        );
        if d.deactivate {
            let _ = svc
                .db
                .set_tracked_active(&t.server_id, &t.repo, t.number, false);
            changed = true;
        }
        if let Some((head, new_changes)) = &d.update_head {
            let _ = svc
                .db
                .set_tracked_head(&t.server_id, &t.repo, t.number, head, *new_changes);
            changed = true;
        }
        if d.auto_review {
            match queue::enqueue(svc, fresh.clone(), ReviewTrigger::Auto) {
                Ok(_) => svc.sink.notify(
                    &format!("New changes on {}#{}", t.repo, t.number),
                    &format!("\"{}\" changed. An auto-review has started.", fresh.title),
                ),
                Err(e) => tracing::warn!("auto-review enqueue failed: {e}"),
            }
        } else if d.notify_new_changes {
            svc.sink.notify(
                &format!("New changes on {}#{}", t.repo, t.number),
                &format!(
                    "\"{}\" has new commits since your last review.",
                    fresh.title
                ),
            );
        }
    }
    if changed {
        svc.sink.tracked_updated();
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
        assert_eq!(
            decide(&t, PrState::Open, "a", true, false),
            Decision::default()
        );
    }

    #[test]
    fn new_commits_alert_and_auto_review_when_not_approved() {
        let t = tracked("a", Some("a"), Some(Verdict::RequestChanges));
        let d = decide(&t, PrState::Open, "b", true, false);
        assert_eq!(d.update_head, Some(("b".into(), true)));
        assert!(d.notify_new_changes && d.auto_review);

        let d = decide(&t, PrState::Open, "b", false, false);
        assert!(d.notify_new_changes && !d.auto_review);
    }

    #[test]
    fn no_auto_review_after_approve_or_while_running() {
        let approved = tracked("a", Some("a"), Some(Verdict::Approve));
        let d = decide(&approved, PrState::Open, "b", true, false);
        assert!(d.notify_new_changes && !d.auto_review);

        let t = tracked("a", Some("a"), Some(Verdict::Comment));
        assert!(!decide(&t, PrState::Open, "b", true, true).auto_review);
    }

    #[test]
    fn closed_or_merged_prs_are_deactivated() {
        let t = tracked("a", Some("a"), None);
        assert!(decide(&t, PrState::Merged, "a", true, false).deactivate);
        assert!(decide(&t, PrState::Closed, "z", true, false).deactivate);
    }

    #[test]
    fn head_matching_last_review_is_not_new() {
        // E.g. a force-push back to the reviewed commit.
        let t = tracked("x", Some("a"), Some(Verdict::Comment));
        let d = decide(&t, PrState::Open, "a", true, false);
        assert_eq!(d.update_head, Some(("a".into(), false)));
        assert!(!d.notify_new_changes && !d.auto_review);
    }
}
