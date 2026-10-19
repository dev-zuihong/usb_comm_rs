//! HID 会话、Report ID 编解码与专用工作线程。

use super::*;
use std::{ffi::CString, thread};

session_api!(
    /// HID 报告会话；输入保留报告边界，payload 不包含 Report ID。
    HidSession, BlockingHidSession, HidReport
);

impl HidSession {
    pub async fn recv(&self, timeout: Option<std::time::Duration>) -> Result<HidReport> {
        match self.core.receive(timeout).await? {
            Input::Hid(report) => Ok(report),
            _ => unreachable!(),
        }
    }
    /// payload 不包含 Report ID；无编号报告传 0，底层统一补入 ID 前缀。
    pub async fn send_report(&self, report_id: u8, payload: &[u8]) -> Result<usize> {
        let data = self.report_bytes(report_id, payload)?;
        self.core
            .write(data, false)
            .await
            .map(|count| count.saturating_sub(1))
    }
    pub async fn send_feature_report(&self, report_id: u8, payload: &[u8]) -> Result<()> {
        let data = self.report_bytes(report_id, payload)?;
        self.core
            .request(Operation::FeatureWrite(data))
            .await
            .map(|_| ())
    }
    /// payload_size 不包含 Report ID；实际返回的 payload 可能短于该上限。
    pub async fn get_feature_report(
        &self,
        report_id: u8,
        payload_size: usize,
    ) -> Result<HidReport> {
        let size = payload_size
            .checked_add(1)
            .ok_or_else(|| Error::InvalidConfig("report size overflow".into()))?;
        self.core.validate_write(size)?;
        match self
            .core
            .request(Operation::FeatureRead {
                id: report_id,
                size,
            })
            .await?
        {
            Output::Bytes(bytes) => Ok(decode_input(bytes, true)),
            _ => unreachable!(),
        }
    }
    pub async fn report_descriptor(&self) -> Result<Vec<u8>> {
        match self.core.request(Operation::Descriptor).await? {
            Output::Bytes(bytes) => Ok(bytes),
            _ => unreachable!(),
        }
    }
    fn report_bytes(&self, id: u8, payload: &[u8]) -> Result<Vec<u8>> {
        self.core.validate_write(
            payload
                .len()
                .checked_add(1)
                .ok_or_else(|| Error::InvalidConfig("report size overflow".into()))?,
        )?;
        let mut data = Vec::with_capacity(payload.len() + 1);
        data.push(id);
        data.extend_from_slice(payload);
        Ok(data)
    }
}

impl BlockingHidSession {
    pub fn send_report(&self, id: u8, payload: &[u8]) -> Result<usize> {
        runtime::check_blocking()?;
        let data = self.0.report_bytes(id, payload)?;
        let core = self.0.core.clone();
        runtime::blocking(async move {
            core.write(data, false)
                .await
                .map(|count| count.saturating_sub(1))
        })
    }
    pub fn send_feature_report(&self, id: u8, payload: &[u8]) -> Result<()> {
        runtime::check_blocking()?;
        let data = self.0.report_bytes(id, payload)?;
        let core = self.0.core.clone();
        runtime::blocking(async move {
            core.request(Operation::FeatureWrite(data))
                .await
                .map(|_| ())
        })
    }
    pub fn get_feature_report(&self, id: u8, payload_size: usize) -> Result<HidReport> {
        let s = self.0.clone();
        runtime::blocking(async move { s.get_feature_report(id, payload_size).await })
    }
    pub fn report_descriptor(&self) -> Result<Vec<u8>> {
        let s = self.0.clone();
        runtime::blocking(async move { s.report_descriptor().await })
    }
}

#[cfg(test)]
mod tests;

pub(crate) async fn open(path: Vec<u8>, options: HidOptions) -> Result<HidSession> {
    options.validate()?;
    let path =
        CString::new(path).map_err(|_| Error::InvalidConfig("HID path contains NUL".into()))?;
    runtime::run(async move {
        let (core, worker) = Core::new(options.io.clone());
        let (ready, startup) = oneshot::channel();
        thread::Builder::new().name("usb-comm-hid".into()).spawn(
            move || match hidapi::HidApi::new()
                .and_then(|api| api.open_path(&path).map(|device| (api, device)))
            {
                Ok((api, device)) => {
                    let _ = ready.send(Ok(()));
                    serve(device, worker, options);
                    drop(api);
                }
                Err(error) => {
                    let error = Error::from(error);
                    worker.lifecycle.finish(error.clone());
                    let _ = ready.send(Err(error));
                }
            },
        )?;
        runtime::timeout(core.options.operation_timeout, async {
            startup.await.map_err(|_| Error::Closed)?
        })
        .await?;
        Ok(HidSession { core })
    })
    .await
}

pub(super) fn decode_input(mut bytes: Vec<u8>, numbered: bool) -> HidReport {
    let report_id = if numbered && !bytes.is_empty() {
        bytes.remove(0)
    } else {
        0
    };
    HidReport {
        report_id,
        payload: bytes,
    }
}

fn serve(device: hidapi::HidDevice, mut worker: Worker, options: HidOptions) {
    let mut buffer = vec![0; options.io.read_buffer_size];
    let mut pending_input = None;
    let error = 'worker: loop {
        if let Some(error) = worker.stop.borrow().clone() {
            break error;
        }
        if worker.stop.has_changed().is_err() {
            break Error::Closed;
        }
        // 每轮只处理有限条命令，防止持续发送导致输入长期得不到处理。
        for _ in 0..8 {
            if worker.stop.borrow().is_some() {
                continue 'worker;
            }
            let command = match worker.commands.try_recv() {
                Ok(command) => command,
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => break 'worker Error::Closed,
            };
            if command.expired() {
                let _ = command.reply.send(Err(Error::Timeout));
                continue;
            }
            let result = execute(&device, command.operation);
            let terminal = matches!(&result, Err(Error::Disconnected(_)));
            let error = result.as_ref().err().cloned();
            let _ = command.reply.send(result);
            if terminal {
                break 'worker error.unwrap();
            }
        }
        if let Some(report) = pending_input.take() {
            match worker.input.try_send(Input::Hid(report)) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(Input::Hid(report))) => {
                    pending_input = Some(report)
                }
                Err(_) => break Error::Closed,
            }
        }
        if pending_input.is_some() {
            // 接收背压时保留一条待交付报告，并继续响应命令和关闭请求。
            thread::sleep(
                options
                    .read_poll_interval
                    .min(std::time::Duration::from_millis(10)),
            );
            continue;
        }
        match device.read_timeout(&mut buffer, options.read_poll_interval.as_millis() as i32) {
            Ok(0) => {}
            Ok(count) => {
                pending_input = Some(decode_input(
                    buffer[..count].to_vec(),
                    options.numbered_input_reports,
                ))
            }
            Err(error) => break Error::from(error),
        }
    };
    drop(device);
    worker.lifecycle.finish(error.clone());
    while let Ok(command) = worker.commands.try_recv() {
        let _ = command.reply.send(Err(error.clone()));
    }
    tracing::debug!(%error, "HID session stopped");
}

fn execute(device: &hidapi::HidDevice, operation: Operation) -> Result<Output> {
    match operation {
        Operation::Write { data, .. } => Ok(Output::Count(device.write(&data)?)),
        Operation::FeatureWrite(data) => {
            device.send_feature_report(&data)?;
            Ok(Output::Unit)
        }
        Operation::FeatureRead { id, size } => {
            let mut buffer = vec![0; size];
            buffer[0] = id;
            let count = device.get_feature_report(&mut buffer)?;
            buffer.truncate(count);
            Ok(Output::Bytes(buffer))
        }
        Operation::Descriptor => {
            let mut buffer = vec![0; hidapi::MAX_REPORT_DESCRIPTOR_SIZE];
            let count = device.get_report_descriptor(&mut buffer)?;
            buffer.truncate(count);
            Ok(Output::Bytes(buffer))
        }
        _ => Err(Error::Unsupported("serial operation on HID device".into())),
    }
}
