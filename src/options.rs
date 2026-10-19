//! 管理器与各传输类型的配置及边界校验。

use std::time::Duration;

use crate::{Error, Result};
pub use serialport::{DataBits, FlowControl, Parity, StopBits};

/// 设备发现及热插拔设置；默认合并 40 ms 通知，每 5 秒校准一次。
#[derive(Debug, Clone)]
pub struct ManagerOptions {
    /// 事件广播队列容量；订阅消费不及时会收到 ResyncRequired。
    pub event_capacity: usize,
    /// 合并密集通知的等待时间，减少驱动节点尚未就绪时的重复枚举。
    pub debounce: Duration,
    /// 低频重新枚举，用于校准快照并覆盖不产生原生通知的虚拟端口。
    pub reconcile_interval: Duration,
    /// macOS 是否同时保留 /dev/tty.*；默认只枚举 /dev/cu.*。
    pub include_macos_dialin: bool,
}

impl Default for ManagerOptions {
    fn default() -> Self {
        Self {
            event_capacity: 256,
            debounce: Duration::from_millis(40),
            reconcile_interval: Duration::from_secs(5),
            include_macos_dialin: false,
        }
    }
}

impl ManagerOptions {
    pub(crate) fn validate(&self) -> Result<()> {
        if !(1..=65_536).contains(&self.event_capacity)
            || self.debounce.is_zero()
            || self.reconcile_interval.is_zero()
            || self.debounce > Duration::from_secs(60)
            || self.reconcile_interval > Duration::from_secs(86_400)
        {
            return Err(Error::InvalidConfig(
                "event capacity must be 1..=65536, debounce 1 ns..=60 s, reconciliation 1 ns..=24 h".into(),
            ));
        }
        Ok(())
    }
}

/// 所有会话共用的队列、缓冲区与超时限制，分配前统一校验。
#[derive(Debug, Clone)]
pub struct IoOptions {
    /// 等待执行的命令条数上限，不包含调用方尚未入队的数据。
    pub command_capacity: usize,
    /// 接收队列条数上限；满时暂停后续读取，形成背压。
    pub receive_capacity: usize,
    /// 单次读取缓冲区上限；HID 按线缆报告大小计算，包含 Report ID。
    pub read_buffer_size: usize,
    /// 单次发送的最大字节数；HID 包含 Report ID。
    pub max_write_size: usize,
    /// 超时覆盖排队与执行；None 表示等待完成或关闭。
    pub operation_timeout: Option<Duration>,
}

impl Default for IoOptions {
    fn default() -> Self {
        Self {
            command_capacity: 64,
            receive_capacity: 64,
            read_buffer_size: 4096,
            max_write_size: 1024 * 1024,
            operation_timeout: Some(Duration::from_secs(5)),
        }
    }
}

impl IoOptions {
    pub(crate) fn validate(&self) -> Result<()> {
        if ![self.command_capacity, self.receive_capacity]
            .into_iter()
            .all(|v| (1..=65_536).contains(&v))
            || ![self.read_buffer_size, self.max_write_size]
                .into_iter()
                .all(|v| (1..=16 * 1024 * 1024).contains(&v))
            || self
                .operation_timeout
                .is_some_and(|v| v.is_zero() || v > Duration::from_secs(86_400))
        {
            return Err(Error::InvalidConfig(
                "capacities must be 1..=65536, buffers 1..=16 MiB, timeout 1 ns..=24 h".into(),
            ));
        }
        Ok(())
    }
}

/// 串口配置，默认为 115200、8N1、无流控。
#[derive(Debug, Clone)]
pub struct SerialConfig {
    pub baud_rate: u32,
    pub data_bits: DataBits,
    pub stop_bits: StopBits,
    pub parity: Parity,
    pub flow_control: FlowControl,
    /// None 沿用驱动默认行为；部分驱动在打开端口时会改变 DTR 电平。
    pub dtr: Option<bool>,
    pub rts: Option<bool>,
}

impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            baud_rate: 115_200,
            data_bits: DataBits::Eight,
            stop_bits: StopBits::One,
            parity: Parity::None,
            flow_control: FlowControl::None,
            dtr: None,
            rts: None,
        }
    }
}

impl SerialConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.baud_rate == 0 {
            return Err(Error::InvalidConfig("baud rate must be positive".into()));
        }
        Ok(())
    }
}

/// HID 报告与读取设置；报告是否带 ID 由调用方按设备描述符指定。
#[derive(Debug, Clone)]
pub struct HidOptions {
    pub io: IoOptions,
    /// 输入报告首字节为 Report ID 时设为 true，接收时剥离一次 ID。
    pub numbered_input_reports: bool,
    /// 专用线程的读取等待时间；hidapi 按整数毫秒接收此参数。
    pub read_poll_interval: Duration,
}

impl Default for HidOptions {
    fn default() -> Self {
        Self {
            io: IoOptions::default(),
            numbered_input_reports: false,
            read_poll_interval: Duration::from_millis(10),
        }
    }
}

impl HidOptions {
    pub(crate) fn validate(&self) -> Result<()> {
        self.io.validate()?;
        // hidapi 的超时参数是 i32 毫秒；不足 1 ms 会截断为忙轮询。
        if !(1..=i32::MAX as u128).contains(&self.read_poll_interval.as_millis()) {
            return Err(Error::InvalidConfig(
                "HID poll interval must be 1..=i32::MAX milliseconds".into(),
            ));
        }
        Ok(())
    }
}

/// USB Print 配置，默认值与 ING PTB（VID 36B0 / PID 5320）一致。
#[derive(Debug, Clone)]
pub struct UsbPrintOptions {
    pub io: IoOptions,
    /// Bulk OUT 端点最大包大小；PTB 为 64 字节，不自动读取描述符。
    pub bulk_max_packet_size: usize,
    /// 非空载荷长度整除最大包大小时，额外提交一次真实零字节 USB 写入。
    pub send_zero_length_packet: bool,
}

impl Default for UsbPrintOptions {
    fn default() -> Self {
        Self {
            io: IoOptions::default(),
            bulk_max_packet_size: 64,
            send_zero_length_packet: true,
        }
    }
}

impl UsbPrintOptions {
    pub(crate) fn validate(&self) -> Result<()> {
        self.io.validate()?;
        if !matches!(self.bulk_max_packet_size, 8 | 16 | 32 | 64 | 512 | 1024) {
            return Err(Error::InvalidConfig(
                "USB bulk max packet size must be 8, 16, 32, 64, 512 or 1024".into(),
            ));
        }
        Ok(())
    }

    #[cfg(any(windows, test))]
    pub(crate) fn needs_zero_length_packet(&self, length: usize) -> bool {
        self.send_zero_length_packet
            && length > 0
            && length.is_multiple_of(self.bulk_max_packet_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_io_limits_are_rejected_before_channel_or_buffer_allocation() {
        for options in [
            IoOptions {
                receive_capacity: 0,
                ..Default::default()
            },
            IoOptions {
                command_capacity: usize::MAX,
                ..Default::default()
            },
            IoOptions {
                read_buffer_size: usize::MAX,
                ..Default::default()
            },
            IoOptions {
                operation_timeout: Some(Duration::MAX),
                ..Default::default()
            },
        ] {
            assert!(matches!(options.validate(), Err(Error::InvalidConfig(_))));
        }
    }

    #[test]
    fn usb_print_zlp_boundaries_match_the_reference_transport() {
        let mut options = UsbPrintOptions::default();
        for (length, expected) in [
            (0, false),
            (1, false),
            (63, false),
            (64, true),
            (65, false),
            (127, false),
            (128, true),
            (512, true),
        ] {
            assert_eq!(options.needs_zero_length_packet(length), expected);
        }
        options.send_zero_length_packet = false;
        assert!(!options.needs_zero_length_packet(64));
        options.send_zero_length_packet = true;
        options.bulk_max_packet_size = 512;
        assert!(!options.needs_zero_length_packet(64));
        assert!(options.needs_zero_length_packet(512));
        options.bulk_max_packet_size = 0;
        assert!(matches!(options.validate(), Err(Error::InvalidConfig(_))));
    }
}
