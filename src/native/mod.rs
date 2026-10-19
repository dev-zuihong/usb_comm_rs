//! 原生热插拔监听只发出刷新信号，设备身份与差异由重新枚举确定。

use crate::{Error, Result};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[derive(Clone)]
pub(crate) struct Signal {
    pub changed: Arc<tokio::sync::Notify>,
    pub failure: Arc<std::sync::Mutex<Option<Error>>>,
}

impl Signal {
    fn notify(&self) {
        self.changed.notify_one();
    }
}

pub(crate) struct Monitor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    pub fn start() -> Result<(Self, Signal)> {
        let stop = Arc::new(AtomicBool::new(false));
        let signal = Signal {
            changed: Arc::new(tokio::sync::Notify::new()),
            failure: Arc::new(std::sync::Mutex::new(None)),
        };
        let (ready, startup) = mpsc::sync_channel(1);
        let thread_stop = stop.clone();
        let thread_signal = signal.clone();
        let thread = thread::Builder::new()
            .name("usb-comm-hotplug".into())
            .spawn(move || {
                #[cfg(windows)]
                let result = windows::run(&thread_stop, &thread_signal, ready);
                #[cfg(target_os = "linux")]
                let result = linux::run(&thread_stop, &thread_signal, ready);
                #[cfg(target_os = "macos")]
                let result = macos::run(&thread_stop, &thread_signal, ready);
                #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
                let result: Result<()> = {
                    let error = Error::Unsupported("native hotplug on this platform".into());
                    let _ = ready.send(Err(error.clone()));
                    Err(error)
                };
                if let Err(error) = result {
                    *thread_signal.failure.lock().unwrap() = Some(error);
                    thread_signal.notify();
                }
            })?;
        let monitor = Self {
            stop,
            thread: Some(thread),
        };
        startup.recv().map_err(|_| Error::Backend {
            backend: "hotplug",
            message: "monitor exited during startup".into(),
        })??;
        Ok((monitor, signal))
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}
