//! 通过 Configuration Manager 监听 Windows 设备接口变化。

use super::*;
use std::{ffi::c_void, ptr, time::Duration};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;

unsafe extern "system" fn callback(
    _handle: HCMNOTIFICATION,
    context: *const c_void,
    _action: CM_NOTIFY_ACTION,
    _data: *const CM_NOTIFY_EVENT_DATA,
    _size: u32,
) -> u32 {
    // SAFETY: context 在注册存续期间由 Box 持有；注销会等待已有回调结束后才释放它。
    let signal = unsafe { &*(context.cast::<Signal>()) };
    signal.notify();
    0
}

pub(super) fn run(
    stop: &AtomicBool,
    signal: &Signal,
    ready: mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    let context = Box::into_raw(Box::new(signal.clone()));
    let filter = CM_NOTIFY_FILTER {
        cbSize: size_of::<CM_NOTIFY_FILTER>() as u32,
        FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
        // 监听全部接口类，覆盖 HID、串口、USB Print 和厂商虚拟串口。
        Flags: CM_NOTIFY_FILTER_FLAG_ALL_INTERFACE_CLASSES,
        ..Default::default()
    };
    let mut handle = ptr::null_mut();
    // SAFETY: 过滤器和句柄有效，context 在成功注销前始终存活。
    let status =
        unsafe { CM_Register_Notification(&filter, context.cast(), Some(callback), &mut handle) };
    if status != CR_SUCCESS {
        unsafe {
            drop(Box::from_raw(context));
        }
        let error = Error::Backend {
            backend: "CM_Register_Notification",
            message: format!("CONFIGRET {status:#x}"),
        };
        let _ = ready.send(Err(error.clone()));
        return Err(error);
    }
    let _ = ready.send(Ok(()));
    while !stop.load(Ordering::Acquire) {
        thread::park_timeout(Duration::from_millis(100));
    }
    // SAFETY: 句柄来自上面的成功注册；只有注销成功后才能释放 context，
    // 否则后续回调仍可能访问它。
    let status = unsafe { CM_Unregister_Notification(handle) };
    if status == CR_SUCCESS {
        unsafe {
            drop(Box::from_raw(context));
        }
    } else {
        tracing::error!(
            status,
            "hotplug unregister failed; retaining callback context"
        );
    }
    Ok(())
}
