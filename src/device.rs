//! 设备标识、枚举元数据和热插拔事件。

use crate::Error;

/// 可打开的传输类型；复合设备可以同时包含多个不同类型的 endpoint。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Transport {
    Hid,
    Serial,
    /// 直接访问 Windows usbprint.sys 设备接口，不经过系统打印队列。
    UsbPrint,
}

/// 可打开的设备路径；HID 路径保留原始字节，Unix 下不保证是 UTF-8。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DevicePath {
    Hid(Vec<u8>),
    Serial(String),
    UsbPrint(String),
}

/// 路径与连接代次组成的设备标识，只在同一个管理器内有效。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId {
    pub endpoint: DevicePath,
    /// 管理器内的连接代次；观察到移除后再次出现时变化，旧代次不能继续打开。
    pub generation: u64,
}

/// 系统报告的总线类型；无法确认时保留 Unknown。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bus {
    Usb,
    Bluetooth,
    Other,
    Unknown,
}

/// HID 接口的枚举元数据，可用于区分同 VID/PID 的不同集合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidInfo {
    /// USB 接口号；后端无法提供时为 None。
    pub interface_number: Option<i32>,
    /// hidapi 枚举返回的 HID 顶层集合 Usage Page。
    pub usage_page: u16,
    /// 对应 Usage Page 下的顶层集合 Usage，与 Report ID 不同。
    pub usage: u16,
}

/// Windows USB Print 的可选位置和实例信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbPrintInfo {
    pub location: Option<String>,
    /// Windows 设备实例 ID，不一定是 USB 序列号。
    pub device_instance_id: Option<String>,
}

/// 一个可打开 endpoint 的快照，不按 VID/PID 合并复合设备接口。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub transport: Transport,
    pub bus: Bus,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub serial_number: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub hid: Option<HidInfo>,
    pub usb_print: Option<UsbPrintInfo>,
}

/// 快照及增量事件；revision 在同一管理器内递增，同批事件可共享 revision。
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DeviceEvent {
    /// 当前订阅范围的完整列表，作为增量更新的起点。
    Snapshot {
        revision: u64,
        devices: Vec<DeviceInfo>,
    },
    Added {
        revision: u64,
        device: DeviceInfo,
    },
    Removed {
        revision: u64,
        device: DeviceInfo,
    },
    /// 同一连接的元数据发生变化；筛选范围进出会转换为 Added / Removed。
    Changed {
        revision: u64,
        previous: DeviceInfo,
        device: DeviceInfo,
    },
    /// 事件队列已溢出，应重新 discover 并建立订阅；旧队列可能仍含过时事件。
    ResyncRequired {
        missed: u64,
    },
    /// 枚举或原生监听失败，保留上次成功的设备快照。
    BackendError(Error),
}

/// HID 报告，明确分离 Report ID 与 payload。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidReport {
    /// 0 表示无编号报告；payload 不包含此 ID。
    pub report_id: u8,
    pub payload: Vec<u8>,
}

/// 会话状态；关闭请求和资源完全释放是两个不同阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Open,
    /// 已请求关闭，工作线程仍可能正在回收底层请求。
    Closing,
    /// 正常关闭，工作线程已释放底层句柄。
    Closed,
    /// 设备断开或会话遇到终态错误，底层资源可能仍在回收。
    Disconnected,
}
