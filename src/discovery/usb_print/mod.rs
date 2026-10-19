//! USB Print 枚举的平台分派与 VID/PID 解析。

#[cfg(windows)]
mod windows;

use crate::{DeviceInfo, Result};

pub(crate) fn enumerate() -> Result<Vec<DeviceInfo>> {
    #[cfg(windows)]
    {
        windows::enumerate()
    }
    #[cfg(not(windows))]
    {
        Ok(Vec::new())
    }
}

/// USBPRINT 子节点可能没有 VID/PID，Windows 枚举还会查询父设备实例。
/// 接口路径不保证包含真实序列号，不能从路径猜测。
#[cfg(any(windows, test))]
fn parse_vid_pid(value: &str) -> (Option<u16>, Option<u16>) {
    let lower = value.to_ascii_lowercase();
    let mut vendor = None;
    let mut product = None;
    for component in lower.split(['\\', '#', '&']) {
        if let Some(hex) = component.strip_prefix("vid_").filter(|v| v.len() == 4) {
            vendor = u16::from_str_radix(hex, 16).ok().or(vendor);
        }
        if let Some(hex) = component.strip_prefix("pid_").filter(|v| v.len() == 4) {
            product = u16::from_str_radix(hex, 16).ok().or(product);
        }
    }
    (vendor, product)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reference_ptb_paths_and_usb_parent_ids() {
        for path in [
            r"\\?\usb#vid_36b0&pid_5320#ptb#{28d78fad-5a12-11d1-ae5b-0000f803a8c2}",
            r"\\?\USBPRINT#VID_36B0&PID_5320#INGCHIPSUSB_DEVSYS#{28d78fad-5a12-11d1-ae5b-0000f803a8c2}",
            r"USB\VID_36B0&PID_5320&MI_00\PTB_SERIAL",
        ] {
            assert_eq!(parse_vid_pid(path), (Some(0x36b0), Some(0x5320)));
        }
    }

    #[test]
    fn missing_malformed_or_embedded_ids_are_not_guessed() {
        for path in [
            r"\\?\USBPRINT#INGCHIPSUSB_DEVSYS#ptb#{28d78fad-5a12-11d1-ae5b-0000f803a8c2}",
            r"\\?\usb#vid_36b00&pid_53200#device",
            r"\\?\usb#vid_ZZZZ&pid_XXXX#device",
            r"\\?\usb#notvid_36b0&notpid_5320#device",
        ] {
            assert_eq!(parse_vid_pid(path), (None, None));
        }
        assert_eq!(
            parse_vid_pid(r"USB\VID_36B0&PID_0001\x"),
            (Some(0x36b0), Some(1))
        );
    }
}
