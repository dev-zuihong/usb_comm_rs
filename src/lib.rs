//! 跨平台设备发现与 HID / 串口 / Windows USB Print 原始通信。
//!
//! 异步接口使用共享的后台 Tokio 运行时。同步接口拒绝在 Tokio 上下文中阻塞，
//! 此时应使用异步接口。

#![doc = include_str!("../README.md")]

mod device;
mod discovery;
mod error;
mod filter;
mod manager;
mod native;
mod options;
mod runtime;
mod session;

pub use device::*;
pub use error::{Error, Result};
pub use filter::{DeviceFilter, DeviceFilterSet};
pub use manager::{BlockingDeviceWatcher, DeviceManager, DeviceWatcher};
pub use options::*;
pub use session::{
    BlockingHidSession, BlockingSerialSession, BlockingUsbPrintSession, HidSession, SerialSession,
    UsbPrintSession,
};
