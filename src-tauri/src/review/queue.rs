//! Concurrency-limited review queue with one job per PR. A newer head SHA supersedes
//! (cancels) an older queued or running review of the same PR.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::db::models::{PullRequest, Review, ReviewStatus, ReviewTrigger};
use crate::error::AppResult;
use crate::review::pipeline;
use crate::service::{ProgressKind, Service};

type PrKey = (String, String, u64);

struct Job {
    review_id: String,
    head_sha: String,
    cancel: CancellationToken,
}

pub struct ReviewQueue {
    semaphore: Arc<Semaphore>,
    jobs: Mutex<HashMap<PrKey, Job>>,
}

fn key(pr: &PullRequest) -> PrKey {
    (pr.server_id.clone(), pr.repo.clone(), pr.number)
}

impl ReviewQueue {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            jobs: Mutex::new(HashMap::new()),
        }
    }

    fn jobs(&self) -> std::sync::MutexGuard<'_, HashMap<PrKey, Job>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn active_count(&self) -> usize {
        self.jobs().len()
    }

    pub fn is_active(&self, server_id: &str, repo: &str, number: u64) -> bool {
        self.jobs()
            .contains_key(&(server_id.to_string(), repo.to_string(), number))
    }

    pub fn cancel(&self, review_id: &str) -> bool {
        let jobs = self.jobs();
        match jobs.values().find(|j| j.review_id == review_id) {
            Some(job) => {
                job.cancel.cancel();
                true
            }
            None => false,
        }
    }

    pub fn cancel_all(&self) {
        for job in self.jobs().values() {
            job.cancel.cancel();
        }
    }
}

/// Queue a review of `pr` (which must be freshly fetched so `head_sha` is current).
pub fn enqueue(svc: &Arc<Service>, pr: PullRequest, trigger: ReviewTrigger) -> AppResult<Review> {
    let k = key(&pr);
    {
        let jobs = svc.queue.jobs();
        if let Some(job) = jobs.get(&k) {
            if job.head_sha == pr.head_sha {
                return svc.db.get_review(&job.review_id);
            }
            job.cancel.cancel();
        }
    }

    svc.db.track_pr(&pr)?;
    let review = svc.db.insert_review(&pr, trigger)?;
    let cancel = CancellationToken::new();
    svc.queue.jobs().insert(
        k.clone(),
        Job {
            review_id: review.id.clone(),
            head_sha: pr.head_sha.clone(),
            cancel: cancel.clone(),
        },
    );
    svc.progress(&review.id, ProgressKind::Status, "Queued");
    svc.publish_review(&review.id);
    svc.sink.tracked_updated();

    let svc2 = svc.clone();
    let review_id = review.id.clone();
    let semaphore = svc.queue.semaphore.clone();
    tokio::spawn(async move {
        let permit = tokio::select! {
            p = semaphore.acquire_owned() => p.ok(),
            _ = cancel.cancelled() => None,
        };
        match permit {
            Some(_permit) => pipeline::run_review(&svc2, &review_id, pr, trigger, cancel).await,
            None => {
                let _ = svc2.db.set_review_status(
                    &review_id,
                    ReviewStatus::Cancelled,
                    Some("Superseded before it started"),
                );
                svc2.progress(&review_id, ProgressKind::Status, "Cancelled");
                svc2.publish_review(&review_id);
            }
        }
        let mut jobs = svc2.queue.jobs();
        if jobs.get(&k).is_some_and(|j| j.review_id == review_id) {
            jobs.remove(&k);
        }
        drop(jobs);
        svc2.sink.tracked_updated();
    });
    Ok(review)
}
