//! Finite batch scheduling. Policy admission and actual probing belong to the caller.
use futures_util::{StreamExt, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchTarget {
    pub id: String,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct BatchConfig {
    /// A snapshot of targets already selected and authorized by the caller.
    pub targets: Vec<BatchTarget>,
    pub concurrency: usize,
    pub rounds: usize,
    pub timeout: Duration,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AttemptOutcome {
    Success,
    Failure(String),
}

/// Admission must use the shared, persistent cooldown service, including waiting
/// and atomic reservation. Execution must update shared 429/503 backoff before
/// returning. Both futures must be cancellation-safe; reservations are not refunded.
/// Execution must not spawn detached work and must release resources when dropped.
pub trait BatchWorker: Sync {
    /// Receives initial and changed item snapshots. Must return promptly and not panic.
    /// Callers may publish these to a watch channel; it is not called after return.
    fn on_update(&self, _item: &BatchItem) {}
    fn admit<'a>(
        &'a self,
        target: &'a BatchTarget,
        round: usize,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
    fn execute<'a>(
        &'a self,
        target: &'a BatchTarget,
        round: usize,
    ) -> Pin<Box<dyn Future<Output = AttemptOutcome> + Send + 'a>>;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ItemStatus {
    Pending,
    Success,
    Failure(String),
    Skipped(String),
    Cancelled,
    TimedOut,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchItem {
    pub target: BatchTarget,
    /// One-based round number.
    pub round: usize,
    /// True only once the execution callback has actually been invoked.
    pub attempted: bool,
    pub status: ItemStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchReport {
    /// Complete means all scheduled items were attempted; failures can still occur.
    pub complete: bool,
    pub items: Vec<BatchItem>,
}

impl BatchConfig {
    fn normalized(&self) -> Result<Vec<BatchTarget>, String> {
        if !(1..=4).contains(&self.concurrency)
            || !(1..=3).contains(&self.rounds)
            || self.timeout.is_zero()
            || self.timeout > Duration::from_secs(120)
        {
            return Err(
                "batch limits: concurrency 1..4, rounds 1..3, timeout > 0 and <= 120 seconds"
                    .into(),
            );
        }
        let mut ids = HashMap::new();
        let mut urls = HashSet::new();
        let mut targets = Vec::new();
        for target in &self.targets {
            if target.id.trim().is_empty() {
                return Err("empty target id".into());
            }
            let parsed = Url::parse(&target.url).map_err(|_| "invalid target URL")?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.fragment().is_some()
            {
                return Err("invalid target URL".into());
            }
            let url = parsed.to_string();
            if let Some(existing) = ids.get(&target.id) {
                if existing != &url {
                    return Err("target id has conflicting URLs".into());
                }
                continue;
            }
            ids.insert(target.id.clone(), url.clone());
            if urls.insert(url.clone()) {
                targets.push(BatchTarget {
                    id: target.id.clone(),
                    url,
                });
            }
        }
        if targets.is_empty() {
            return Err("batch has no targets".into());
        }
        Ok(targets)
    }
}

/// All work is polled within this future, never spawned. Dropping it cancels all
/// callbacks; use the token and await the result to obtain a partial report.
pub async fn run_batch<W: BatchWorker>(
    config: BatchConfig,
    worker: &W,
    cancel: CancellationToken,
) -> Result<BatchReport, String> {
    let targets = config.normalized()?;
    let mut items: Vec<_> = (1..=config.rounds)
        .flat_map(|round| {
            targets.iter().cloned().map(move |target| BatchItem {
                target,
                round,
                attempted: false,
                status: ItemStatus::Pending,
            })
        })
        .collect();
    for item in &items {
        worker.on_update(item);
    }
    let deadline = Instant::now() + config.timeout;
    let mut interrupted = None;
    for round in 1..=config.rounds {
        let start = (round - 1) * targets.len();
        let mut next = 0;
        let mut active = FuturesUnordered::new();
        let mut phases: HashMap<usize, Arc<AtomicBool>> = HashMap::new();
        loop {
            while next < targets.len()
                && active.len() < config.concurrency
                && !cancel.is_cancelled()
                && Instant::now() < deadline
            {
                let index = start + next;
                next += 1;
                let phase = Arc::new(AtomicBool::new(false));
                phases.insert(index, phase.clone());
                let target = &targets[index - start];
                active.push(async move {
                    let status = match worker.admit(target, round).await {
                        Err(reason) => ItemStatus::Skipped(reason),
                        Ok(()) => {
                            phase.store(true, Ordering::SeqCst);
                            worker.on_update(&BatchItem {
                                target: target.clone(),
                                round,
                                attempted: true,
                                status: ItemStatus::Pending,
                            });
                            match worker.execute(target, round).await {
                                AttemptOutcome::Success => ItemStatus::Success,
                                AttemptOutcome::Failure(reason) => ItemStatus::Failure(reason),
                            }
                        }
                    };
                    (index, status)
                });
            }
            if active.is_empty() && next == targets.len() {
                break;
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => { interrupted = Some(ItemStatus::Cancelled); break; }
                _ = tokio::time::sleep_until(deadline) => { interrupted = Some(ItemStatus::TimedOut); break; }
                result = active.next(), if !active.is_empty() => {
                    if let Some((index, status)) = result {
                        items[index].attempted = phases.remove(&index).unwrap().load(Ordering::SeqCst);
                        items[index].status = status;
                        worker.on_update(&items[index]);
                    }
                }
            }
        }
        // Drop callbacks before returning/starting any next round.
        drop(active);
        if let Some(status) = interrupted.clone() {
            for (index, phase) in phases {
                items[index].attempted = phase.load(Ordering::SeqCst);
            }
            for item in &mut items {
                if item.status == ItemStatus::Pending {
                    item.status = status.clone();
                    worker.on_update(item);
                }
            }
            break;
        }
    }
    let complete = items.iter().all(|item| {
        item.attempted && !matches!(item.status, ItemStatus::Cancelled | ItemStatus::TimedOut)
    });
    Ok(BatchReport { complete, items })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    struct Fake {
        log: Mutex<Vec<String>>,
        running: AtomicUsize,
        max: AtomicUsize,
        block: bool,
    }
    impl Fake {
        fn new(block: bool) -> Self {
            Self {
                log: Mutex::new(vec![]),
                running: AtomicUsize::new(0),
                max: AtomicUsize::new(0),
                block,
            }
        }
    }
    impl BatchWorker for Fake {
        fn admit<'a>(
            &'a self,
            target: &'a BatchTarget,
            round: usize,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
            Box::pin(async move {
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("admit:{round}:{}", target.id));
                Ok(())
            })
        }
        fn execute<'a>(
            &'a self,
            target: &'a BatchTarget,
            round: usize,
        ) -> Pin<Box<dyn Future<Output = AttemptOutcome> + Send + 'a>> {
            Box::pin(async move {
                let n = self.running.fetch_add(1, Ordering::SeqCst) + 1;
                self.max.fetch_max(n, Ordering::SeqCst);
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("start:{round}:{}", target.id));
                if self.block {
                    std::future::pending::<()>().await;
                }
                tokio::task::yield_now().await;
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("end:{round}:{}", target.id));
                self.running.fetch_sub(1, Ordering::SeqCst);
                if target.id == "a" {
                    AttemptOutcome::Failure("429/503 backoff recorded".into())
                } else {
                    AttemptOutcome::Success
                }
            })
        }
    }
    fn cfg() -> BatchConfig {
        BatchConfig {
            targets: vec![
                BatchTarget {
                    id: "a".into(),
                    url: "https://EXAMPLE.com".into(),
                },
                BatchTarget {
                    id: "a".into(),
                    url: "https://example.com/".into(),
                },
                BatchTarget {
                    id: "duplicate-url".into(),
                    url: "https://example.com/".into(),
                },
                BatchTarget {
                    id: "b".into(),
                    url: "https://other.example/".into(),
                },
            ],
            concurrency: 2,
            rounds: 2,
            timeout: Duration::from_secs(10),
        }
    }
    #[tokio::test]
    async fn deduplicates_obeys_concurrency_and_barrier() {
        let fake = Fake::new(false);
        let report = run_batch(cfg(), &fake, CancellationToken::new())
            .await
            .unwrap();
        assert!(report.complete);
        assert_eq!(report.items.len(), 4);
        assert_eq!(fake.max.load(Ordering::SeqCst), 2);
        let log = fake.log.lock().unwrap();
        let second = log.iter().position(|s| s.starts_with("admit:2:")).unwrap();
        assert_eq!(
            log[..second]
                .iter()
                .filter(|s| s.starts_with("end:1:"))
                .count(),
            2
        );
        assert!(matches!(report.items[0].status, ItemStatus::Failure(_)));
    }
    #[tokio::test(start_paused = true)]
    async fn timeout_and_cancel_do_not_invent_attempts() {
        let fake = Fake::new(true);
        let mut config = cfg();
        config.concurrency = 1;
        let report = run_batch(config.clone(), &fake, CancellationToken::new())
            .await
            .unwrap();
        assert!(!report.complete);
        assert!(report.items[0].attempted);
        assert!(
            report.items[1..]
                .iter()
                .all(|i| !i.attempted && i.status == ItemStatus::TimedOut)
        );
        let token = CancellationToken::new();
        token.cancel();
        let report = run_batch(config, &fake, token).await.unwrap();
        assert!(
            report
                .items
                .iter()
                .all(|i| !i.attempted && i.status == ItemStatus::Cancelled)
        );
    }
    #[test]
    fn rejects_invalid_bounds_and_conflicting_ids() {
        let mut config = cfg();
        config.concurrency = 5;
        assert!(config.normalized().is_err());
        config = cfg();
        config.rounds = 4;
        assert!(config.normalized().is_err());
        config = cfg();
        config.timeout = Duration::from_secs(121);
        assert!(config.normalized().is_err());
        config = cfg();
        config.targets[1].url = "https://different.example/".into();
        assert!(config.normalized().is_err());
    }
}
