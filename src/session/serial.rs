//! 串口会话与异步全双工 I/O。

use super::*;
use std::{future::poll_fn, io, task::Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_serial::{SerialPort, SerialStream};

session_api!(
    /// 串口字节流会话；克隆共享收发队列，多个接收者竞争输入。
    SerialSession, BlockingSerialSession, Vec<u8>
);

impl SerialSession {
    /// 返回原始字节流片段；片段边界不代表业务帧边界。
    pub async fn recv(&self, timeout: Option<std::time::Duration>) -> Result<Vec<u8>> {
        match self.core.receive(timeout).await? {
            Input::Serial(bytes) => Ok(bytes),
            _ => Err(Error::Backend {
                backend: "serial",
                message: "unexpected input type".into(),
            }),
        }
    }

    /// 执行一次写入，可能只写入部分缓冲区，返回实际接受的字节数。
    pub async fn write(&self, bytes: &[u8]) -> Result<usize> {
        self.core.write(self.core.write_buffer(bytes)?, false).await
    }
    /// 持续处理部分写入，直到 OS 接受整个缓冲区；成功不代表远端已确认。
    pub async fn write_all(&self, bytes: &[u8]) -> Result<()> {
        self.core
            .write(self.core.write_buffer(bytes)?, true)
            .await
            .map(|_| ())
    }

    /// 配置逐项应用；驱动报错时可能已有部分设置生效，不执行原子回滚。
    pub async fn configure(&self, config: SerialConfig) -> Result<()> {
        self.core
            .request(Operation::Configure(config))
            .await
            .map(|_| ())
    }
    pub async fn set_lines(&self, dtr: Option<bool>, rts: Option<bool>) -> Result<()> {
        self.core
            .request(Operation::Lines { dtr, rts })
            .await
            .map(|_| ())
    }
}

impl BlockingSerialSession {
    pub fn write(&self, bytes: &[u8]) -> Result<usize> {
        runtime::check_blocking()?;
        let data = self.0.core.write_buffer(bytes)?;
        let core = self.0.core.clone();
        runtime::blocking(async move { core.write(data, false).await })
    }
    pub fn write_all(&self, bytes: &[u8]) -> Result<()> {
        runtime::check_blocking()?;
        let data = self.0.core.write_buffer(bytes)?;
        let core = self.0.core.clone();
        runtime::blocking(async move { core.write(data, true).await.map(|_| ()) })
    }
    pub fn configure(&self, config: SerialConfig) -> Result<()> {
        let s = self.0.clone();
        runtime::blocking(async move { s.configure(config).await })
    }
    pub fn set_lines(&self, dtr: Option<bool>, rts: Option<bool>) -> Result<()> {
        let s = self.0.clone();
        runtime::blocking(async move { s.set_lines(dtr, rts).await })
    }
}

#[cfg(test)]
mod tests;

pub(crate) async fn open(
    path: String,
    config: SerialConfig,
    options: IoOptions,
) -> Result<SerialSession> {
    options.validate()?;
    config.validate()?;
    runtime::run(async move {
        let handle = tokio::runtime::Handle::current();
        let port = runtime::timeout(options.operation_timeout, async move {
            tokio::task::spawn_blocking(move || {
                let _entered = handle.enter();
                let mut builder = serialport::new(&path, config.baud_rate)
                    .data_bits(config.data_bits)
                    .stop_bits(config.stop_bits)
                    .parity(config.parity)
                    .flow_control(config.flow_control);
                if let Some(dtr) = config.dtr {
                    builder = builder.dtr_on_open(dtr);
                }
                let mut port = SerialStream::open(&builder)?;
                if let Some(rts) = config.rts {
                    port.write_request_to_send(rts)?;
                }
                Ok::<_, Error>(port)
            })
            .await
            .map_err(|e| Error::Backend {
                backend: "serial open",
                message: e.to_string(),
            })?
        })
        .await?;
        let (core, worker) = Core::new(options.clone());
        tokio::spawn(async move {
            serve(port, worker, options.read_buffer_size).await;
        });
        Ok(SerialSession { core })
    })
    .await
}

fn configure(port: &mut SerialStream, config: SerialConfig) -> Result<()> {
    config.validate()?;
    port.set_baud_rate(config.baud_rate)?;
    port.set_data_bits(config.data_bits)?;
    port.set_stop_bits(config.stop_bits)?;
    port.set_parity(config.parity)?;
    port.set_flow_control(config.flow_control)?;
    lines(port, config.dtr, config.rts)
}

fn lines(port: &mut SerialStream, dtr: Option<bool>, rts: Option<bool>) -> Result<()> {
    if let Some(value) = dtr {
        port.write_data_terminal_ready(value)?;
    }
    if let Some(value) = rts {
        port.write_request_to_send(value)?;
    }
    Ok(())
}

struct PendingWrite {
    data: Vec<u8>,
    all: bool,
    offset: usize,
    reply: oneshot::Sender<Result<Output>>,
    deadline: Option<tokio::time::Instant>,
}

struct Progress {
    read: Option<io::Result<usize>>,
    write: Option<io::Result<usize>>,
}

trait Port: AsyncRead + AsyncWrite + Unpin {
    fn configure(&mut self, _config: SerialConfig) -> Result<()> {
        Err(Error::Unsupported("configuration on this transport".into()))
    }
    fn lines(&mut self, _dtr: Option<bool>, _rts: Option<bool>) -> Result<()> {
        Err(Error::Unsupported("modem lines on this transport".into()))
    }
}

impl Port for SerialStream {
    fn configure(&mut self, config: SerialConfig) -> Result<()> {
        configure(self, config)
    }
    fn lines(&mut self, dtr: Option<bool>, rts: Option<bool>) -> Result<()> {
        lines(self, dtr, rts)
    }
}

async fn serve(mut port: impl Port, mut worker: Worker, buffer_size: usize) {
    let mut buffer = vec![0; buffer_size];
    let mut pending_input = None;
    let mut pending_write: Option<PendingWrite> = None;
    let error = loop {
        if let Some(reason) = worker.stop.borrow().clone() {
            break reason;
        }
        if pending_write.as_ref().is_some_and(|v| {
            v.reply.is_closed() || v.deadline.is_some_and(|d| d <= tokio::time::Instant::now())
        }) && let Some(write) = pending_write.take()
        {
            let _ = write.reply.send(Err(Error::Timeout));
        }
        let deadline = pending_write.as_ref().and_then(|v| v.deadline);
        tokio::select! {
            _ = worker.stop.changed() => { break worker.stop.borrow().clone().unwrap_or(Error::Closed); },
            // 同一任务内同时轮询读写；写入阻塞时仍可接收，配置操作使用同一句柄。
            progress = poll_fn(|cx| {
                let mut read = None;
                let mut write = None;
                if pending_input.is_none() {
                    let mut buf = ReadBuf::new(&mut buffer);
                    if let Poll::Ready(result) = std::pin::Pin::new(&mut port).poll_read(cx, &mut buf) {
                        read = Some(result.map(|()| buf.filled().len()));
                    }
                }
                if let Some(pending) = &pending_write
                    && let Poll::Ready(result) = std::pin::Pin::new(&mut port).poll_write(cx, &pending.data[pending.offset..]) {
                    write = Some(result);
                }
                if read.is_some() || write.is_some() { Poll::Ready(Progress { read, write }) } else { Poll::Pending }
            }) => {
                if let Some(result) = progress.write {
                    match result {
                        Ok(0) => break Error::from(io::Error::new(io::ErrorKind::WriteZero, "serial write returned zero")),
                        Ok(count) => {
                            let pending = pending_write.as_mut().unwrap();
                            pending.offset += count;
                            if !pending.all || pending.offset == pending.data.len() {
                                let pending = pending_write.take().unwrap();
                                let _ = pending.reply.send(Ok(Output::Count(pending.offset)));
                            }
                        }
                        Err(error) => break error.into(),
                    }
                }
                if let Some(result) = progress.read {
                    match result {
                        Ok(0) => break Error::Disconnected("serial EOF".into()),
                        Ok(count) => pending_input = Some(buffer[..count].to_vec()),
                        Err(error) => break error.into(),
                    }
                }
            },
            permit = worker.input.reserve(), if pending_input.is_some() => {
                match permit {
                    Ok(permit) => permit.send(Input::Serial(pending_input.take().unwrap())),
                    Err(_) => break Error::Closed,
                }
            },
            command = worker.commands.recv(), if pending_write.is_none() => {
                let Some(command) = command else { break Error::Closed; };
                if command.expired() { let _ = command.reply.send(Err(Error::Timeout)); continue; }
                match command.operation {
                    Operation::Write { data, all } => {
                        if data.is_empty() { let _ = command.reply.send(Ok(Output::Count(0))); }
                        else { pending_write = Some(PendingWrite { data, all, offset: 0,
                            reply: command.reply, deadline: command.deadline }); }
                    }
                    Operation::Configure(config) => { let _ = command.reply.send(port.configure(config).map(|()| Output::Unit)); }
                    Operation::Lines { dtr, rts } => { let _ = command.reply.send(port.lines(dtr, rts).map(|()| Output::Unit)); }
                    _ => { let _ = command.reply.send(Err(Error::Unsupported("HID operation on serial port".into()))); }
                }
            },
            _ = runtime::sleep_until(deadline) => {},
            // 调用方取消后会关闭响应通道；即使 OS 读写均空闲且没有超时，也需定期检查。
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)), if pending_write.is_some() => {},
        }
    };
    drop(port);
    worker.lifecycle.finish(error.clone());
    if let Some(write) = pending_write {
        let _ = write.reply.send(Err(error.clone()));
    }
    while let Ok(command) = worker.commands.try_recv() {
        let _ = command.reply.send(Err(error.clone()));
    }
    tracing::debug!(%error, "serial session stopped");
}
