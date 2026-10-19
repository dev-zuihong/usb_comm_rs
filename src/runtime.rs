//! 共享后台运行时，以及同步桥接、取消传播和超时辅助函数。

use crate::{Error, Result};
use std::{future::Future, sync::OnceLock, time::Duration};
use tokio::{
    runtime::{Handle, Runtime},
    sync::oneshot,
};

pub(crate) fn handle() -> Result<Handle> {
    // 运行时由进程共享，避免在调用方的异步上下文中析构 Tokio Runtime。
    static RUNTIME: OnceLock<std::io::Result<Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(
                    std::thread::available_parallelism().map_or(2, |v| v.get().clamp(2, 8)),
                )
                .thread_name("usb-comm-io")
                .enable_all()
                .build()
        })
        .as_ref()
        .map(|v| v.handle().clone())
        .map_err(|v| Error::Backend {
            backend: "runtime",
            message: v.to_string(),
        })
}

pub(crate) fn check_blocking() -> Result<()> {
    if Handle::try_current().is_ok() {
        Err(Error::InvalidContext)
    } else {
        Ok(())
    }
}

pub(crate) async fn run<T: Send + 'static>(
    future: impl Future<Output = Result<T>> + Send + 'static,
) -> Result<T> {
    let (mut tx, rx) = oneshot::channel();
    handle()?.spawn(async move {
        tokio::select! {
            biased;
            // 调用方丢弃 future 后取消后台等待，使未执行命令通过关闭的响应通道失效。
            _ = tx.closed() => {},
            result = future => { let _ = tx.send(result); }
        }
    });
    rx.await.map_err(|_| Error::Closed)?
}

pub(crate) fn blocking<T: Send + 'static>(
    future: impl Future<Output = Result<T>> + Send + 'static,
) -> Result<T> {
    check_blocking()?;
    let (tx, rx) = oneshot::channel();
    handle()?.spawn(async move {
        let _ = tx.send(future.await);
    });
    rx.blocking_recv().map_err(|_| Error::Closed)?
}

pub(crate) async fn timeout<T>(
    duration: Option<Duration>,
    future: impl Future<Output = Result<T>>,
) -> Result<T> {
    match duration {
        Some(duration) => tokio::time::timeout(duration, future)
            .await
            .map_err(|_| Error::Timeout)?,
        None => future.await,
    }
}

/// 可选截止时间的等待分支；没有截止时间时保持 Pending，供 select! 使用。
pub(crate) async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
