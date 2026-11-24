//! Concurrency-limited review queue with one job per PR. A newer head SHA supersedes
//! (cancels) an older queued or running review of the same PR.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
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

struct Limit {
    max: usize,
    /// Permits still held by running reviews that must be retired (not returned) when those
    /// reviews finish, because the limit was lowered while they ran.
    debt: usize,
}

pub struct ReviewQueue {
    semaphore: Arc<Semaphore>,
    limit: Mutex<Limit>,
    jobs: Mutex<HashMap<PrKey, Job>>,
}

fn key(pr: &PullRequest) -> PrKey {
    (pr.server_id.clone(), pr.repo.clone(), pr.number)
}

impl ReviewQueue {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
            limit: Mutex::new(Limit {
                max: max_concurrent.max(1),
                debt: 0,
            }),
            jobs: Mutex::new(HashMap::new()),
        }
    }

    fn jobs(&self) -> std::sync::MutexGuard<'_, HashMap<PrKey, Job>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
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

    /// Changes how many reviews may run at once. Lowering the limit lets running reviews
    /// finish; their permits are retired as they are released.
    pub fn set_limit(&self, max_concurrent: usize) {
        let new = max_concurrent.max(1);
        let mut limit = self.limit.lock().unwrap_or_else(PoisonError::into_inner);
        if new > limit.max {
            let mut extra = new - limit.max;
            let repaid = extra.min(limit.debt);
            limit.debt -= repaid;
            extra -= repaid;
            self.semaphore.add_permits(extra);
        } else if new < limit.max {
            let surplus = limit.max - new;
            let forgotten = self.semaphore.forget_permits(surplus);
            limit.debt += surplus - forgotten;
        }
        limit.max = new;
    }

    /// Returns a finished review's permit, or retires it if the limit was lowered meanwhile.
    fn release(&self, permit: OwnedSemaphorePermit) {
        let mut limit = self.limit.lock().unwrap_or_else(PoisonError::into_inner);
        if limit.debt > 0 {
            limit.debt -= 1;
            permit.forget();
        }
    }

    /// Cancels every queued or running review of a server (e.g. when it is deleted).
    pub fn cancel_server(&self, server_id: &str) {
        for (k, job) in self.jobs().iter() {
            if k.0 == server_id {
                job.cancel.cancel();
            }
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
    let cancel = CancellationToken::new();
    // Check, insert the review and register the job under one lock, so a manual click and a
    // watcher auto-review of the same head can't both start a review.
    let review = {
        let mut jobs = svc.queue.jobs();
        if let Some(job) = jobs.get(&k) {
            if job.head_sha == pr.head_sha {
                return svc.db.get_review(&job.review_id);
            }
            job.cancel.cancel();
        }
        svc.db.track_pr(&pr)?;
        let review = svc.db.insert_review(&pr, trigger)?;
        jobs.insert(
            k.clone(),
            Job {
                review_id: review.id.clone(),
                head_sha: pr.head_sha.clone(),
                cancel: cancel.clone(),
            },
        );
        review
    };
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
            Some(permit) => {
                pipeline::run_review(&svc2, &review_id, pr, trigger, cancel).await;
                svc2.queue.release(permit);
            }
            None => {
                svc2.record_review_status(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn limit_changes_apply_to_new_reviews() {
        let q = ReviewQueue::new(2);
        let a = q.semaphore.clone().acquire_owned().await.unwrap();
        let b = q.semaphore.clone().acquire_owned().await.unwrap();
        // Lower to 1 while two run, then raise to 3 before either finishes.
        q.set_limit(1);
        q.set_limit(3);
        assert_eq!(q.semaphore.available_permits(), 1);
        q.release(a);
        q.release(b);
        assert_eq!(q.semaphore.available_permits(), 3);

        q.set_limit(1);
        assert_eq!(q.semaphore.available_permits(), 1);
        let c = q.semaphore.clone().acquire_owned().await.unwrap();
        q.set_limit(2);
        q.release(c);
        assert_eq!(q.semaphore.available_permits(), 2);
    }
}
