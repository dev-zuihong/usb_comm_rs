//! Windows USB Printer 字节流会话，整包发送可补一个真实 ZLP。

use super::*;
use std::time::Duration;

#[cfg(windows)]
mod windows;

pub(crate) async fn open(path: String, options: UsbPrintOptions) -> Result<UsbPrintSession> {
    options.validate()?;
    #[cfg(windows)]
    {
        windows::open(path, options).await
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(Error::Unsupported(
            "USB Print uses the Windows usbprint.sys driver".into(),
        ))
    }
}

session_api!(
    /// 双向 USB Printer 字节流；一次 write_all 包含载荷和可选的终止 ZLP。
    UsbPrintSession, BlockingUsbPrintSession, Vec<u8>
);

impl UsbPrintSession {
    /// 返回原始字节流片段；USB 传输边界不代表业务帧边界。
    pub async fn recv(&self, timeout: Option<Duration>) -> Result<Vec<u8>> {
        match self.core.receive(timeout).await? {
            Input::UsbPrint(data) => Ok(data),
            _ => Err(Error::Backend {
                backend: "usbprint",
                message: "unexpected input type".into(),
            }),
        }
    }

    /// 完成载荷写入，并按配置为整包载荷补一个真实 ZLP。
    /// 空载荷不提交数据包；成功不代表设备已确认。
    pub async fn write_all(&self, data: &[u8]) -> Result<()> {
        self.core
            .write(self.core.write_buffer(data)?, true)
            .await
            .map(|_| ())
    }
}

impl BlockingUsbPrintSession {
    pub fn write_all(&self, data: &[u8]) -> Result<()> {
        runtime::check_blocking()?;
        let data = self.0.core.write_buffer(data)?;
        let core = self.0.core.clone();
        runtime::blocking(async move { core.write(data, true).await.map(|_| ()) })
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unsupported_platform_returns_a_typed_error() {
        assert!(matches!(
            open("test".into(), UsbPrintOptions::default()).await,
            Err(Error::Unsupported(_))
        ));
    }
}
