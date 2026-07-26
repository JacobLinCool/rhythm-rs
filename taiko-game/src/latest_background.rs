//! A bounded latest-request-wins worker for blocking GUI support work.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result};

const EVENT_CAPACITY: usize = 1;

pub(crate) enum BackgroundCompletion<T> {
    Completed(Box<T>),
    Cancelled,
    Failed(anyhow::Error),
}

pub(crate) struct BackgroundEvent<I, T> {
    pub(crate) identity: I,
    pub(crate) completion: BackgroundCompletion<T>,
}

type BackgroundJob<T> = Box<dyn FnOnce(&AtomicBool) -> Result<Option<T>> + Send + 'static>;

struct QueuedJob<I, T> {
    identity: I,
    job: BackgroundJob<T>,
}

struct RunningJob<I, T> {
    identity: I,
    cancelled: Arc<AtomicBool>,
    events: Receiver<BackgroundEvent<I, T>>,
    thread: JoinHandle<()>,
}

/// Runs no more than one blocking job at once and retains at most one newer job.
///
/// Starting while a worker is blocked marks that worker cancelled and replaces
/// the single queued slot. Cancellation never joins a live thread.
pub(crate) struct LatestBackgroundTask<I, T> {
    thread_name: &'static str,
    running: Option<RunningJob<I, T>>,
    queued: Option<QueuedJob<I, T>>,
}

impl<I, T> LatestBackgroundTask<I, T>
where
    I: Copy + Eq + Send + 'static,
    T: Send + 'static,
{
    pub(crate) fn new(thread_name: &'static str) -> Self {
        Self {
            thread_name,
            running: None,
            queued: None,
        }
    }

    pub(crate) fn start(
        &mut self,
        identity: I,
        job: impl FnOnce(&AtomicBool) -> Result<Option<T>> + Send + 'static,
    ) -> Result<()> {
        let queued = QueuedJob {
            identity,
            job: Box::new(job),
        };
        if let Some(running) = &self.running {
            running.cancelled.store(true, Ordering::Release);
            self.queued = Some(queued);
            return Ok(());
        }

        self.spawn(queued)
    }

    pub(crate) fn cancel(&mut self) {
        self.queued = None;
        if let Some(running) = &self.running {
            running.cancelled.store(true, Ordering::Release);
        }
    }

    pub(crate) fn poll(&mut self) -> Vec<BackgroundEvent<I, T>> {
        let mut events = Vec::with_capacity(2);
        let Some(running) = self.running.as_ref() else {
            self.start_queued_or_report_failure(&mut events);
            return events;
        };

        let mut worker_finished = false;
        match running.events.try_recv() {
            Ok(event) => {
                events.push(event);
                worker_finished = true;
            }
            Err(TryRecvError::Disconnected) => {
                worker_finished = true;
            }
            Err(TryRecvError::Empty) => {}
        }

        if worker_finished {
            let running = self.running.take().expect("worker was just observed");
            let identity = running.identity;
            let worker_panicked = running.thread.join().is_err();
            if !events.iter().any(|event| event.identity == identity) {
                let message = if worker_panicked {
                    "background worker panicked"
                } else {
                    "background worker exited without a completion"
                };
                events.push(BackgroundEvent {
                    identity,
                    completion: BackgroundCompletion::Failed(anyhow::anyhow!(message)),
                });
            }
            self.start_queued_or_report_failure(&mut events);
        }

        events
    }

    fn spawn(&mut self, queued: QueuedJob<I, T>) -> Result<()> {
        let identity = queued.identity;
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (event_tx, event_rx) = mpsc::sync_channel(EVENT_CAPACITY);
        let thread = thread::Builder::new()
            .name(self.thread_name.to_owned())
            .spawn(move || {
                let completion = match (queued.job)(&worker_cancelled) {
                    Ok(Some(value)) => BackgroundCompletion::Completed(Box::new(value)),
                    Ok(None) => BackgroundCompletion::Cancelled,
                    Err(error) => BackgroundCompletion::Failed(error),
                };
                let _ = event_tx.send(BackgroundEvent {
                    identity,
                    completion,
                });
            })
            .with_context(|| format!("failed to start {} worker", self.thread_name))?;
        self.running = Some(RunningJob {
            identity,
            cancelled,
            events: event_rx,
            thread,
        });
        Ok(())
    }

    fn start_queued_or_report_failure(&mut self, events: &mut Vec<BackgroundEvent<I, T>>) {
        let Some(queued) = self.queued.take() else {
            return;
        };
        let identity = queued.identity;
        if let Err(error) = self.spawn(queued) {
            events.push(BackgroundEvent {
                identity,
                completion: BackgroundCompletion::Failed(error),
            });
        }
    }
}

impl<I, T> Drop for LatestBackgroundTask<I, T> {
    fn drop(&mut self) {
        self.queued = None;
        if let Some(running) = &self.running {
            running.cancelled.store(true, Ordering::Release);
        }
    }
}

pub(crate) fn event_is_current<I: Copy + Eq, T>(
    current: Option<I>,
    event: &BackgroundEvent<I, T>,
) -> bool {
    current == Some(event.identity)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, TryRecvError};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn start_returns_without_waiting_for_blocking_work() {
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let mut task = LatestBackgroundTask::<u64, ()>::new("nonblocking-test");

        let started_at = Instant::now();
        task.start(1, move |_| {
            release_rx.recv().expect("release worker");
            Ok(None)
        })
        .expect("start worker");
        assert!(started_at.elapsed() < Duration::from_millis(100));

        release_tx.send(()).expect("release worker");
        let deadline = Instant::now() + Duration::from_secs(2);
        while task.poll().is_empty() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn superseded_jobs_never_run_concurrently() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum_active = Arc::new(AtomicUsize::new(0));
        let (first_release_tx, first_release_rx) = mpsc::sync_channel(0);
        let (second_started_tx, second_started_rx) = mpsc::sync_channel(0);
        let mut task = LatestBackgroundTask::<u64, ()>::new("single-worker-test");

        let first_active = Arc::clone(&active);
        let first_maximum = Arc::clone(&maximum_active);
        task.start(1, move |_| {
            let count = first_active.fetch_add(1, Ordering::SeqCst) + 1;
            first_maximum.fetch_max(count, Ordering::SeqCst);
            first_release_rx.recv().expect("release first");
            first_active.fetch_sub(1, Ordering::SeqCst);
            Ok(None)
        })
        .expect("start first");

        let second_active = Arc::clone(&active);
        let second_maximum = Arc::clone(&maximum_active);
        task.start(2, move |_| {
            let count = second_active.fetch_add(1, Ordering::SeqCst) + 1;
            second_maximum.fetch_max(count, Ordering::SeqCst);
            second_started_tx.send(()).expect("announce second");
            second_active.fetch_sub(1, Ordering::SeqCst);
            Ok(None)
        })
        .expect("queue second");
        assert!(matches!(
            second_started_rx.try_recv(),
            Err(TryRecvError::Empty)
        ));

        first_release_tx.send(()).expect("release first");
        let deadline = Instant::now() + Duration::from_secs(2);
        while second_started_rx.try_recv().is_err() {
            let _ = task.poll();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        while task.poll().is_empty() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(maximum_active.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn identity_rejects_stale_completion() {
        let stale = BackgroundEvent {
            identity: 4_u64,
            completion: BackgroundCompletion::<()>::Cancelled,
        };
        let current = BackgroundEvent {
            identity: 5_u64,
            completion: BackgroundCompletion::<()>::Cancelled,
        };

        assert!(!event_is_current(Some(5), &stale));
        assert!(event_is_current(Some(5), &current));
        assert!(!event_is_current(None, &current));
    }

    #[test]
    fn cancel_and_drop_do_not_join_a_blocked_worker() {
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let mut task = LatestBackgroundTask::<u64, ()>::new("nonjoining-cancel-test");
        task.start(1, move |_| {
            release_rx.recv().expect("release worker");
            Ok(None)
        })
        .expect("start worker");

        let started_at = Instant::now();
        task.cancel();
        drop(task);
        assert!(started_at.elapsed() < Duration::from_millis(100));
        release_tx.send(()).expect("release detached worker");
    }

    #[test]
    fn cancel_discards_the_queued_retry() {
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let (retry_started_tx, retry_started_rx) = mpsc::sync_channel(0);
        let mut task = LatestBackgroundTask::<u64, ()>::new("cancel-retry-test");
        task.start(1, move |_| {
            release_rx.recv().expect("release worker");
            Ok(None)
        })
        .expect("start worker");
        task.start(2, move |_| {
            retry_started_tx.send(()).expect("announce retry");
            Ok(None)
        })
        .expect("queue retry");

        task.cancel();
        release_tx.send(()).expect("release worker");
        let deadline = Instant::now() + Duration::from_secs(2);
        while task.poll().is_empty() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(matches!(
            retry_started_rx.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
    }
}
