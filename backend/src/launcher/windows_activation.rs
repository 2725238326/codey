//! Keep cleanup ownership until native activation has actually returned.
use std::future::Future;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::oneshot;

pub(super) struct ActivationSupervisor {
    pub cancelled: Arc<AtomicBool>,
    pub timeout: Duration,
    pub settle_timeout: Duration,
}

impl ActivationSupervisor {
    pub async fn run<T, A, F, S, SF, C>(
        self,
        activation: A,
        helper_failure: F,
        mut stop: S,
        finish: C,
        mut reply: oneshot::Sender<Result<T>>,
    ) -> Result<()>
    where
        A: Future<Output = Result<T>>,
        F: Future<Output = anyhow::Error>,
        S: FnMut() -> SF,
        SF: Future<Output = Result<()>>,
        C: FnOnce() -> Result<()>,
    {
        tokio::pin!(activation);
        let interrupted = tokio::select! {
            error = helper_failure => error,
            _ = reply.closed() => anyhow::anyhow!("Windows Store 启动请求已取消"),
            result = tokio::time::timeout(self.timeout, &mut activation) => {
                match result {
                    Ok(Ok(value)) => {
                        if let Err(error) = finish() {
                            let stopped = stop().await;
                            let _ = reply.send(Err(super::platform::startup_activation_error_after_cleanup(
                                anyhow::anyhow!("Windows Store 启动环境清理失败"), stopped, Err(error),
                            )));
                        } else if let Err(undelivered) = reply.send(Ok(value)) {
                            // Keep the returned process handle alive until cleanup ends.
                            let stopped = stop().await;
                            drop(undelivered);
                            stopped?;
                        }
                        return Ok(());
                    }
                    Ok(Err(error)) => {
                        let stopped = stop().await;
                        let _ = reply.send(Err(super::platform::startup_activation_error_after_cleanup(
                            error, stopped, finish(),
                        )));
                        return Ok(());
                    }
                    Err(elapsed) => anyhow::Error::new(elapsed).context("等待 Windows Store 激活超时"),
                }
            }
        };

        // Stop suspended targets to release ActivateApplication. Do not cancel
        // its Rust future: the underlying blocking COM call would keep running.
        self.cancelled.store(true, Ordering::Release);
        let first_stop = stop().await;
        let mut reply = Some(reply);
        let settled = match tokio::time::timeout(self.settle_timeout, &mut activation).await {
            Ok(result) => result,
            Err(_) => {
                let detail = first_stop
                    .as_ref()
                    .err()
                    .map(|error| format!("；进程清理：{error:#}"))
                    .unwrap_or_default();
                let _ = reply.take().unwrap().send(Err(anyhow::anyhow!(
                    "{interrupted:#}{detail}；系统激活尚未结束，后台继续等待并清理，已停止重试和原生恢复"
                )));
                // The finish closure owns the package journal and lock. No new
                // launch may change these settings while a late call is pending.
                loop {
                    tokio::select! {
                        result = &mut activation => break result,
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {
                            // A late debugger can fail before resuming its target.
                            // Sweep again so that suspended target cannot keep COM waiting.
                            let _ = stop().await;
                        }
                    }
                }
            }
        };
        // A process can be created after the first sweep. Recheck only after
        // activation returns, retaining any returned process handle meanwhile.
        let stopped = stop().await;
        drop(settled);
        let cleared = finish();
        if let Some(reply) = reply {
            let _ = reply.send(Err(
                super::platform::startup_activation_error_after_cleanup(
                    interrupted,
                    stopped,
                    cleared,
                ),
            ));
        } else {
            stopped?;
            cleared?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::recovery::IntegrationFailure;
    use super::*;
    use std::sync::{Mutex, atomic::AtomicUsize};

    fn supervisor() -> ActivationSupervisor {
        ActivationSupervisor {
            cancelled: Arc::new(AtomicBool::new(false)),
            timeout: Duration::from_millis(10),
            settle_timeout: Duration::from_millis(5),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn success_clears_settings_before_returning_without_stopping_target() {
        let cleared = AtomicBool::new(false);
        let (reply, response) = oneshot::channel();
        supervisor()
            .run(
                async { Ok(42) },
                std::future::pending(),
                || async { panic!("successful target must remain running") },
                || {
                    cleared.store(true, Ordering::Release);
                    Ok(())
                },
                reply,
            )
            .await
            .unwrap();
        assert!(cleared.load(Ordering::Acquire));
        assert_eq!(response.await.unwrap().unwrap(), 42);
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_retains_exclusive_lock_and_cleans_a_late_process() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("launch.lock");
        let lock = std::fs::File::create(&path).unwrap();
        fs2::FileExt::try_lock_exclusive(&lock).unwrap();
        let contender = std::fs::File::open(&path).unwrap();
        let (complete, activation) = oneshot::channel();
        let (reply, response) = oneshot::channel();
        let alive = Arc::new(AtomicBool::new(false));
        let stopped = alive.clone();
        let task = tokio::spawn(supervisor().run(
            async { activation.await.unwrap() },
            std::future::pending(),
            move || {
                stopped.store(false, Ordering::Release);
                async { Ok(()) }
            },
            move || {
                drop(lock);
                Ok(())
            },
            reply,
        ));
        let error = response.await.unwrap().unwrap_err();
        assert!(!error.is::<IntegrationFailure>());
        assert!(!task.is_finished());
        assert!(fs2::FileExt::try_lock_exclusive(&contender).is_err());
        alive.store(true, Ordering::Release);
        complete.send(Ok(42)).unwrap();
        task.await.unwrap().unwrap();
        assert!(!alive.load(Ordering::Acquire));
        fs2::FileExt::try_lock_exclusive(&contender).unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn deferred_sweep_stops_a_late_suspended_target_before_com_returns() {
        let (complete, activation) = oneshot::channel::<Result<u32>>();
        let complete = Arc::new(Mutex::new(Some(complete)));
        let alive = Arc::new(AtomicBool::new(false));
        let cleared = Arc::new(AtomicBool::new(false));
        let stop_alive = alive.clone();
        let finished = cleared.clone();
        let (reply, response) = oneshot::channel();
        let task = tokio::spawn(supervisor().run(
            async { activation.await.unwrap() },
            std::future::pending(),
            move || {
                if stop_alive.swap(false, Ordering::AcqRel) {
                    complete
                        .lock()
                        .unwrap()
                        .take()
                        .unwrap()
                        .send(Err(anyhow::anyhow!("target terminated")))
                        .unwrap();
                }
                async { Ok(()) }
            },
            move || {
                finished.store(true, Ordering::Release);
                Ok(())
            },
            reply,
        ));
        assert!(response.await.unwrap().is_err());
        assert!(!cleared.load(Ordering::Acquire));
        alive.store(true, Ordering::Release);
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!alive.load(Ordering::Acquire));
        assert!(cleared.load(Ordering::Acquire));
    }

    #[tokio::test(start_paused = true)]
    async fn helper_failure_only_becomes_recoverable_after_activation_and_cleanup_end() {
        let (complete, activation) = oneshot::channel::<Result<u32>>();
        let mut complete = Some(complete);
        let cleared = AtomicBool::new(false);
        let stops = AtomicUsize::new(0);
        let (reply, response) = oneshot::channel();
        supervisor()
            .run(
                async { activation.await.unwrap() },
                async { anyhow::anyhow!("helper validation failed") },
                || {
                    stops.fetch_add(1, Ordering::Relaxed);
                    if let Some(complete) = complete.take() {
                        let _ = complete.send(Ok(42));
                    }
                    async { Ok(()) }
                },
                || {
                    cleared.store(true, Ordering::Release);
                    Ok(())
                },
                reply,
            )
            .await
            .unwrap();
        let error = response.await.unwrap().unwrap_err();
        assert!(error.is::<IntegrationFailure>());
        assert!(format!("{error:#}").contains("helper validation failed"));
        assert!(cleared.load(Ordering::Acquire));
        assert_eq!(stops.load(Ordering::Relaxed), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_caller_still_waits_for_activation_and_cleans_target() {
        let (complete, activation) = oneshot::channel::<Result<u32>>();
        let mut complete = Some(complete);
        let cleared = AtomicBool::new(false);
        let state = supervisor();
        let cancelled = state.cancelled.clone();
        let (reply, response) = oneshot::channel();
        drop(response);
        state
            .run(
                async { activation.await.unwrap() },
                std::future::pending(),
                || {
                    if let Some(complete) = complete.take() {
                        let _ = complete.send(Ok(42));
                    }
                    async { Ok(()) }
                },
                || {
                    cleared.store(true, Ordering::Release);
                    Ok(())
                },
                reply,
            )
            .await
            .unwrap();
        assert!(cancelled.load(Ordering::Acquire));
        assert!(cleared.load(Ordering::Acquire));
    }

    #[tokio::test(start_paused = true)]
    async fn unsuccessful_cleanup_cannot_enable_native_recovery() {
        for stop_fails in [true, false] {
            let (reply, response) = oneshot::channel::<Result<u32>>();
            supervisor()
                .run(
                    async { Err(anyhow::anyhow!("activation failed")) },
                    std::future::pending(),
                    || async {
                        anyhow::ensure!(!stop_fails, "process still running");
                        Ok(())
                    },
                    || {
                        anyhow::ensure!(stop_fails, "settings still active");
                        Ok(())
                    },
                    reply,
                )
                .await
                .unwrap();
            assert!(
                !response
                    .await
                    .unwrap()
                    .unwrap_err()
                    .is::<IntegrationFailure>()
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn settled_timeout_preserves_retry_type_and_native_recovery_eligibility() {
        let (complete, activation) = oneshot::channel::<Result<u32>>();
        let mut complete = Some(complete);
        let (reply, response) = oneshot::channel();
        supervisor()
            .run(
                async { activation.await.unwrap() },
                std::future::pending(),
                || {
                    if let Some(complete) = complete.take() {
                        let _ = complete.send(Ok(42));
                    }
                    async { Ok(()) }
                },
                || Ok(()),
                reply,
            )
            .await
            .unwrap();
        let error = response.await.unwrap().unwrap_err();
        assert!(error.is::<tokio::time::error::Elapsed>());
        assert!(error.is::<IntegrationFailure>());
    }
}
