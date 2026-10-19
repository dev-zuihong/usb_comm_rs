//! 会话公共机制：有界命令队列、取消/超时、接收背压和关闭状态。
//!
//! 各传输模块只实现自己的数据操作；此处统一状态与同步包装，避免语义漂移。

// 三种传输共享完全相同的生命周期接口；宏只展开固定方法，不隐藏后端 I/O。
macro_rules! session_api {
    ($(#[$doc:meta])* $session:ident, $blocking:ident, $packet:ty) => {
        $(#[$doc])*
        #[derive(Clone)]
        pub struct $session {
            pub(crate) core: Arc<Core>,
        }

        impl $session {
            /// 返回所有会话克隆共享的当前状态。
            pub fn state(&self) -> SessionState {
                self.core.state()
            }

            /// 返回首次记录的关闭或断开原因。
            pub fn terminal_error(&self) -> Option<Error> {
                self.core.lifecycle.state.lock().unwrap().1.clone()
            }

            /// 转为同步接口，继续共享同一会话资源。
            pub fn into_blocking(self) -> $blocking {
                $blocking(self)
            }

            /// 请求关闭并等待工作线程释放句柄；重复调用安全。
            pub async fn close(&self) -> Result<()> {
                self.core.close().await
            }
        }

        /// 同步会话包装；与异步接口共享状态、队列和底层句柄。
        #[derive(Clone)]
        pub struct $blocking($session);

        impl $blocking {
            /// 转为异步接口，不重新打开设备。
            pub fn into_async(self) -> $session {
                self.0
            }

            pub fn state(&self) -> SessionState {
                self.0.state()
            }

            pub fn terminal_error(&self) -> Option<Error> {
                self.0.terminal_error()
            }

            /// 阻塞等待关闭；在 Tokio 上下文中返回 InvalidContext。
            pub fn close(&self) -> Result<()> {
                let session = self.0.clone();
                runtime::blocking(async move { session.close().await })
            }

            /// 从共享队列接收一条输入；None 表示无限等待。
            pub fn recv(&self, timeout: Option<std::time::Duration>) -> Result<$packet> {
                let session = self.0.clone();
                runtime::blocking(async move { session.recv(timeout).await })
            }
        }
    };
}

mod hid;
mod serial;
mod usb_print;
pub(crate) use usb_print::open as open_usb_print;
pub use usb_print::{BlockingUsbPrintSession, UsbPrintSession};

use crate::{runtime, *};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, watch};

pub(crate) use hid::open as open_hid;
pub use hid::{BlockingHidSession, HidSession};
pub(crate) use serial::open as open_serial;
pub use serial::{BlockingSerialSession, SerialSession};

pub(super) enum Input {
    Serial(Vec<u8>),
    Hid(HidReport),
    #[cfg_attr(not(windows), allow(dead_code))]
    UsbPrint(Vec<u8>),
}
pub(super) enum Operation {
    Write {
        data: Vec<u8>,
        all: bool,
    },
    Configure(SerialConfig),
    Lines {
        dtr: Option<bool>,
        rts: Option<bool>,
    },
    FeatureWrite(Vec<u8>),
    FeatureRead {
        id: u8,
        size: usize,
    },
    Descriptor,
}
pub(super) enum Output {
    Unit,
    Count(usize),
    Bytes(Vec<u8>),
}
pub(super) struct Command {
    operation: Operation,
    reply: oneshot::Sender<Result<Output>>,
    deadline: Option<tokio::time::Instant>,
}
impl Command {
    fn expired(&self) -> bool {
        self.reply.is_closed()
            || self
                .deadline
                .is_some_and(|v| v <= tokio::time::Instant::now())
    }
}

pub(super) struct Lifecycle {
    state: Mutex<(SessionState, Option<Error>)>,
    done: watch::Sender<bool>,
}

impl Lifecycle {
    fn error(&self) -> Error {
        self.state
            .lock()
            .unwrap()
            .1
            .clone()
            .unwrap_or(Error::Closed)
    }

    fn finish(&self, error: Error) {
        let mut state = self.state.lock().unwrap();
        // 关闭请求可能已记录更具体的原因，工作线程退出时保留首次原因。
        if state.1.is_none() {
            state.1 = Some(error);
        }
        state.0 = if matches!(state.1, Some(Error::Closed)) {
            SessionState::Closed
        } else {
            SessionState::Disconnected
        };
        self.done.send_replace(true);
    }
}

pub(crate) struct Core {
    commands: mpsc::Sender<Command>,
    input: Arc<tokio::sync::Mutex<mpsc::Receiver<Input>>>,
    stop: watch::Sender<Option<Error>>,
    lifecycle: Arc<Lifecycle>,
    options: IoOptions,
    /// 原生事件工作线程的唤醒回调，用于通知命令、关闭或接收队列腾出空间。
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

pub(super) struct Worker {
    commands: mpsc::Receiver<Command>,
    input: mpsc::Sender<Input>,
    stop: watch::Receiver<Option<Error>>,
    lifecycle: Arc<Lifecycle>,
}

impl Core {
    fn new(options: IoOptions) -> (Arc<Self>, Worker) {
        let (commands, command_rx) = mpsc::channel(options.command_capacity);
        let (input_tx, input) = mpsc::channel(options.receive_capacity);
        let (stop, stop_rx) = watch::channel(None);
        let (done, _) = watch::channel(false);
        let lifecycle = Arc::new(Lifecycle {
            state: Mutex::new((SessionState::Open, None)),
            done,
        });
        let worker = Worker {
            commands: command_rx,
            input: input_tx,
            stop: stop_rx,
            lifecycle: lifecycle.clone(),
        };
        (
            Arc::new(Self {
                commands,
                input: Arc::new(tokio::sync::Mutex::new(input)),
                stop,
                lifecycle,
                options,
                wake: None,
            }),
            worker,
        )
    }

    pub(crate) fn disconnect(&self, reason: &str) {
        self.stop_with(Error::Disconnected(reason.into()));
    }

    fn stop_with(&self, reason: Error) {
        let mut state = self.lifecycle.state.lock().unwrap();
        if state.0 == SessionState::Open {
            state.0 = if matches!(reason, Error::Closed) {
                SessionState::Closing
            } else {
                SessionState::Disconnected
            };
            state.1 = Some(reason.clone());
            self.stop.send_replace(Some(reason));
            self.notify_worker();
        }
    }

    fn state(&self) -> SessionState {
        self.lifecycle.state.lock().unwrap().0
    }

    fn notify_worker(&self) {
        if let Some(wake) = &self.wake {
            wake();
        }
    }

    async fn request(self: &Arc<Self>, operation: Operation) -> Result<Output> {
        let core = self.clone();
        runtime::run(async move {
            let mut stop = core.stop.subscribe();
            if core.state() != SessionState::Open {
                return Err(core.lifecycle.error());
            }
            let timeout = core.options.operation_timeout;
            runtime::timeout(timeout, async {
                let (reply, response) = oneshot::channel();
                let command = Command {
                    operation,
                    reply,
                    deadline: timeout.map(|v| tokio::time::Instant::now() + v),
                };
                tokio::select! {
                    biased;
                    _ = stop.changed() => Err(core.lifecycle.error()),
                    result = async {
                        core.commands.send(command).await.map_err(|_| core.lifecycle.error())?;
                        core.notify_worker();
                        response.await.map_err(|_| core.lifecycle.error())?
                    } => result,
                }
            })
            .await
        })
        .await
    }

    async fn receive(self: &Arc<Self>, timeout: Option<std::time::Duration>) -> Result<Input> {
        let core = self.clone();
        runtime::run(async move {
            let mut stop = core.stop.subscribe();
            if core.state() != SessionState::Open {
                return Err(core.lifecycle.error());
            }
            runtime::timeout(timeout, async {
                tokio::select! {
                    biased;
                    _ = stop.changed() => Err(core.lifecycle.error()),
                    value = async { core.input.lock().await.recv().await } => {
                        core.notify_worker();
                        value.ok_or_else(|| core.lifecycle.error())
                    },
                }
            })
            .await
        })
        .await
    }

    async fn close(self: &Arc<Self>) -> Result<()> {
        self.stop_with(Error::Closed);
        let core = self.clone();
        runtime::run(async move {
            let mut done = core.lifecycle.done.subscribe();
            runtime::timeout(core.options.operation_timeout, async {
                while !*done.borrow_and_update() {
                    done.changed().await.map_err(|_| Error::Closed)?;
                }
                Ok(())
            })
            .await
        })
        .await
    }

    fn validate_write(&self, size: usize) -> Result<()> {
        if size > self.options.max_write_size {
            Err(Error::InvalidConfig("write exceeds max_write_size".into()))
        } else {
            Ok(())
        }
    }

    // 在分配前检查大小；同步接口把这份缓冲区直接移入队列，避免二次复制。
    fn write_buffer(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        self.validate_write(bytes.len())?;
        Ok(bytes.to_vec())
    }

    // 只接收已经通过大小校验的缓冲区，三种传输共用写入结果解包。
    async fn write(self: &Arc<Self>, data: Vec<u8>, all: bool) -> Result<usize> {
        match self.request(Operation::Write { data, all }).await? {
            Output::Count(count) => Ok(count),
            _ => unreachable!(),
        }
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.stop_with(Error::Closed);
    }
}
