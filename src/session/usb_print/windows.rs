//! Windows USB Print 重叠 I/O：独立读写、事件唤醒、ZLP 和取消回收。

use super::*;
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr, thread,
};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::*,
    System::{IO::*, Threading::*},
};

#[cfg(test)]
mod tests;

struct Event(OwnedHandle);
impl Event {
    fn new(manual_reset: bool) -> Result<Self> {
        // SAFETY: 使用默认安全属性创建匿名事件，句柄由本包装独占持有。
        let handle = unsafe { CreateEventW(ptr::null(), manual_reset.into(), 0, ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Self(unsafe { OwnedHandle::from_raw_handle(handle) }))
    }
    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle()
    }
    fn signal(&self) {
        // SAFETY: 所有可能调用此回调的对象均共享事件的所有权。
        if unsafe { SetEvent(self.raw()) } == 0 {
            tracing::error!(error = %io::Error::last_os_error(), "USB Print wake failed");
        }
    }
}

/// 持有缓冲区和 OVERLAPPED，直到 Windows 完成或取消该请求。
/// Box 保持 OVERLAPPED 地址稳定，请求未结束时禁止调整缓冲区大小。
struct Transfer {
    file: OwnedHandle,
    event: Event,
    overlapped: Box<OVERLAPPED>,
    buffer: Vec<u8>,
    pending: bool,
    #[cfg(test)]
    zero_length_writes: Arc<std::sync::atomic::AtomicUsize>,
}

impl Transfer {
    fn open(path: &[u16], write: bool, size: usize) -> Result<Self> {
        // SAFETY: 路径以 NUL 结尾，CreateFile 同步返回；读写句柄按参考实现共享访问。
        let file = unsafe {
            CreateFileW(
                path.as_ptr(),
                if write { GENERIC_WRITE } else { GENERIC_READ },
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | if write { FILE_FLAG_WRITE_THROUGH } else { 0 },
                ptr::null_mut(),
            )
        };
        if file == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error().into());
        }
        let file = unsafe { OwnedHandle::from_raw_handle(file) };
        let event = Event::new(true)?;
        let overlapped = Box::new(OVERLAPPED {
            hEvent: event.raw(),
            ..Default::default()
        });
        Ok(Self {
            file,
            event,
            overlapped,
            buffer: vec![0; size],
            pending: false,
            #[cfg(test)]
            zero_length_writes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    fn reset(&mut self) -> Result<()> {
        debug_assert!(!self.pending);
        // SAFETY: 当前没有 I/O 使用此结构，事件句柄仍由本对象持有。
        if unsafe { ResetEvent(self.event.raw()) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        *self.overlapped = OVERLAPPED {
            hEvent: self.event.raw(),
            ..Default::default()
        };
        Ok(())
    }

    fn submitted(&mut self, success: i32) -> Result<()> {
        if success == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error.into());
            }
        }
        self.pending = true;
        Ok(())
    }

    fn read(&mut self) -> Result<()> {
        self.reset()?;
        // SAFETY: 缓冲区与 OVERLAPPED 在请求完成或取消回收前保持存活且地址不变；Drop 也遵守此约定。
        let result = unsafe {
            ReadFile(
                self.file.as_raw_handle(),
                self.buffer.as_mut_ptr(),
                self.buffer.len() as u32,
                ptr::null_mut(),
                &mut *self.overlapped,
            )
        };
        self.submitted(result)
    }

    fn write(&mut self, offset: usize, zero_length: bool) -> Result<()> {
        self.reset()?;
        #[cfg(test)]
        if zero_length {
            self.zero_length_writes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let (buffer, length) = if zero_length {
            (ptr::null(), 0)
        } else {
            (
                self.buffer[offset..].as_ptr(),
                (self.buffer.len() - offset) as u32,
            )
        };
        // SAFETY: 缓冲区与 OVERLAPPED 地址稳定；即使长度为零也传入有效 OVERLAPPED，
        // 使 usbprint.sys 收到真实零长度写请求。
        let result = unsafe {
            WriteFile(
                self.file.as_raw_handle(),
                buffer,
                length,
                ptr::null_mut(),
                &mut *self.overlapped,
            )
        };
        self.submitted(result)
    }

    fn completion(&mut self) -> Option<Result<usize>> {
        if !self.pending {
            return None;
        }
        let mut transferred = 0;
        // SAFETY: 查询使用的句柄和 OVERLAPPED 与尚未完成的请求完全一致。
        if unsafe {
            GetOverlappedResult(
                self.file.as_raw_handle(),
                &*self.overlapped,
                &mut transferred,
                0,
            )
        } != 0
        {
            self.pending = false;
            Some(Ok(transferred as usize))
        } else {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_IO_INCOMPLETE as i32) {
                None
            } else {
                self.pending = false;
                Some(Err(error.into()))
            }
        }
    }

    fn cancel_and_drain(&mut self) {
        if self.pending {
            // SAFETY: 取消不等于完成，释放内存前必须等待内核请求结束。
            // 即使 CancelIoEx 与成功完成发生竞争也需回收；不能取消的驱动只会阻塞此专用线程。
            unsafe {
                CancelIoEx(self.file.as_raw_handle(), &*self.overlapped);
                let mut count = 0;
                GetOverlappedResult(self.file.as_raw_handle(), &*self.overlapped, &mut count, 1);
            }
            self.pending = false;
        }
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        self.cancel_and_drain();
    }
}

pub(super) async fn open(path: String, options: UsbPrintOptions) -> Result<UsbPrintSession> {
    let mut path: Vec<u16> = path.encode_utf16().collect();
    if path.is_empty() || path.contains(&0) {
        return Err(Error::InvalidConfig(
            "USB Print path is empty or contains NUL".into(),
        ));
    }
    path.push(0);
    runtime::run(async move {
        let wake = Arc::new(Event::new(false)?);
        let (mut core, worker) = Core::new(options.io.clone());
        let callback_wake = wake.clone();
        Arc::get_mut(&mut core).unwrap().wake = Some(Arc::new(move || callback_wake.signal()));
        let (ready, startup) = oneshot::channel();
        thread::Builder::new()
            .name("usb-comm-usbprint".into())
            .spawn(move || {
                let opened = Transfer::open(&path, false, options.io.read_buffer_size)
                    .and_then(|read| Transfer::open(&path, true, 0).map(|write| (read, write)));
                match opened {
                    Ok((read, write)) => {
                        let _ = ready.send(Ok(()));
                        serve(read, write, worker, &options, &wake);
                    }
                    Err(error) => {
                        worker.lifecycle.finish(error.clone());
                        let _ = ready.send(Err(error));
                    }
                }
            })?;
        runtime::timeout(core.options.operation_timeout, async {
            startup.await.map_err(|_| Error::Closed)?
        })
        .await?;
        Ok(UsbPrintSession { core })
    })
    .await
}

struct PendingWrite {
    reply: oneshot::Sender<Result<Output>>,
    deadline: Option<tokio::time::Instant>,
    length: usize,
    offset: usize,
    zero_length: bool,
}

fn serve(
    mut read: Transfer,
    mut write: Transfer,
    mut worker: Worker,
    options: &UsbPrintOptions,
    wake: &Event,
) {
    let mut pending_input = None;
    let mut pending_write: Option<PendingWrite> = None;
    let error = 'session: loop {
        if let Some(error) = worker.stop.borrow().clone() {
            break error;
        }
        if worker.stop.has_changed().is_err() {
            break Error::Closed;
        }
        let mut progressed = false;
        if pending_write.as_ref().is_some_and(|v| {
            v.reply.is_closed() || v.deadline.is_some_and(|d| d <= tokio::time::Instant::now())
        }) {
            write.cancel_and_drain();
            if let Some(pending) = pending_write.take() {
                let _ = pending.reply.send(Err(Error::Timeout));
            }
            progressed = true;
        }
        if let Some(result) = write.completion() {
            let count = match result {
                Ok(count) => count,
                Err(error) => break error,
            };
            let pending = pending_write.as_mut().unwrap();
            if pending.zero_length {
                if count != 0 {
                    break Error::Backend {
                        backend: "usbprint",
                        message: "nonzero ZLP completion size".into(),
                    };
                }
                let pending = pending_write.take().unwrap();
                let _ = pending.reply.send(Ok(Output::Count(pending.length)));
            } else {
                if count == 0 || count > pending.length - pending.offset {
                    break Error::from(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "invalid USB Print write completion",
                    ));
                }
                pending.offset += count;
                if pending.offset == pending.length {
                    if options.needs_zero_length_packet(pending.length) {
                        pending.zero_length = true;
                    } else {
                        let pending = pending_write.take().unwrap();
                        let _ = pending.reply.send(Ok(Output::Count(pending.length)));
                    }
                }
            }
            progressed = true;
        }
        if let Some(result) = read.completion() {
            let count = match result {
                Ok(count) => count,
                Err(error) => break error,
            };
            if count == 0 {
                break Error::Disconnected("USB Print EOF".into());
            }
            if count > read.buffer.len() {
                break Error::Backend {
                    backend: "usbprint",
                    message: "invalid read completion size".into(),
                };
            }
            pending_input = Some(read.buffer[..count].to_vec());
            progressed = true;
        }
        if let Some(data) = pending_input.take() {
            match worker.input.try_send(Input::UsbPrint(data)) {
                Ok(()) => progressed = true,
                Err(mpsc::error::TrySendError::Full(Input::UsbPrint(data))) => {
                    pending_input = Some(data)
                }
                Err(_) => break Error::Closed,
            }
        }
        if pending_write.is_none() {
            // 每轮命令批次有上限，保证输入和关闭请求能及时得到处理。
            for _ in 0..8 {
                let command = match worker.commands.try_recv() {
                    Ok(command) => command,
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => break 'session Error::Closed,
                };
                progressed = true;
                if command.expired() {
                    let _ = command.reply.send(Err(Error::Timeout));
                    continue;
                }
                match command.operation {
                    Operation::Write { data, .. } => {
                        if data.is_empty() {
                            let _ = command.reply.send(Ok(Output::Count(0)));
                            continue;
                        }
                        let length = data.len();
                        write.buffer = data;
                        pending_write = Some(PendingWrite {
                            reply: command.reply,
                            deadline: command.deadline,
                            length,
                            offset: 0,
                            zero_length: false,
                        });
                        break;
                    }
                    _ => {
                        let _ = command.reply.send(Err(Error::Unsupported(
                            "HID/serial configuration on USB Print".into(),
                        )));
                    }
                }
            }
        }
        if let Some(pending) = &pending_write
            && !write.pending
        {
            if let Err(error) = write.write(pending.offset, pending.zero_length) {
                break error;
            }
            progressed = true;
        }
        if pending_input.is_none() && !read.pending {
            if let Err(error) = read.read() {
                break error;
            }
            progressed = true;
        }
        if progressed {
            continue;
        }
        // 最多等待三个事件，使用栈上数组避免每轮等待都分配 Vec。
        let mut events = [wake.raw(); 3];
        let mut event_count = 1;
        if read.pending {
            events[event_count] = read.event.raw();
            event_count += 1;
        }
        if write.pending {
            events[event_count] = write.event.raw();
            event_count += 1;
        }
        // 调用方取消可能只关闭响应通道而不触发原生事件。
        // 仅在存在活动写入时，定期检查取消和截止时间。
        let timeout = if let Some(pending) = &pending_write {
            pending.deadline.map_or(20, |v| {
                v.saturating_duration_since(tokio::time::Instant::now())
                    .as_millis()
                    .min(20) as u32
            })
        } else {
            INFINITE
        };
        // SAFETY: 只传入数组中已填充的事件句柄，等待期间句柄均保持有效。
        if unsafe { WaitForMultipleObjects(event_count as u32, events.as_ptr(), 0, timeout) }
            == WAIT_FAILED
        {
            break io::Error::last_os_error().into();
        }
    };
    // 先回收未完成请求并释放句柄，再发布关闭完成，保证 close 成功时资源已释放。
    drop(read);
    drop(write);
    worker.lifecycle.finish(error.clone());
    if let Some(pending) = pending_write {
        let _ = pending.reply.send(Err(error.clone()));
    }
    while let Ok(command) = worker.commands.try_recv() {
        let _ = command.reply.send(Err(error.clone()));
    }
    tracing::debug!(%error, "USB Print session stopped");
}
