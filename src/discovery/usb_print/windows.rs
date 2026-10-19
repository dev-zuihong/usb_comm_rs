//! 通过 SetupAPI 枚举 usbprint.sys，并向父 USB 节点补全设备标识。

use super::parse_vid_pid;
use crate::*;
use std::{mem::size_of, ptr};
use windows_sys::{
    Win32::{Devices::DeviceAndDriverInstallation::*, Foundation::*},
    core::GUID,
};

const USB_PRINT_INTERFACE: GUID = GUID::from_u128(0x28d78fad_5a12_11d1_ae5b_0000f803a8c2);

struct DeviceSet(HDEVINFO);
impl Drop for DeviceSet {
    fn drop(&mut self) {
        // SAFETY: 本对象持有 SetupDiGetClassDevsW 成功返回的设备信息集。
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

pub(super) fn enumerate() -> Result<Vec<DeviceInfo>> {
    // SAFETY: 类 GUID 有效，此调用不需要父窗口或枚举器。
    let set = unsafe {
        SetupDiGetClassDevsW(
            &USB_PRINT_INTERFACE,
            ptr::null(),
            ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if set == INVALID_HANDLE_VALUE as isize {
        return Err(std::io::Error::last_os_error().into());
    }
    let set = DeviceSet(set);
    let mut devices = Vec::new();
    for index in 0.. {
        let mut interface = SP_DEVICE_INTERFACE_DATA {
            cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: 输出结构已设置所需大小，设备信息集保持有效。
        if unsafe {
            SetupDiEnumDeviceInterfaces(
                set.0,
                ptr::null(),
                &USB_PRINT_INTERFACE,
                index,
                &mut interface,
            )
        } == 0
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_ITEMS as i32) {
                break;
            }
            return Err(error.into());
        }
        let mut info = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        let mut required = 0;
        // SAFETY: 仅查询所需大小，不传入详情缓冲区。
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &interface,
                ptr::null_mut(),
                0,
                &mut required,
                &mut info,
            );
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) || required < 6 {
            return Err(error.into());
        }
        // 使用 u64 存储保证变长 Windows 结构的对齐。
        // 路径在 32/64 位下都从第 4 字节开始，cbSize 则分别是 6/8。
        let mut storage = vec![0_u64; (required as usize).div_ceil(size_of::<u64>())];
        let detail = storage
            .as_mut_ptr()
            .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        // SAFETY: 已为结构头和路径分配足够大且满足对齐要求的存储。
        unsafe {
            ptr::write_unaligned(
                ptr::addr_of_mut!((*detail).cbSize),
                size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32,
            );
        }
        // SAFETY: 传入字节数与分配的变长缓冲区大小一致。
        if unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &interface,
                detail,
                required,
                ptr::null_mut(),
                &mut info,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: 上一次成功调用已初始化从第 4 字节开始的 UTF-16 路径。
        let path = unsafe {
            std::slice::from_raw_parts(
                storage.as_ptr().cast::<u8>().add(4).cast::<u16>(),
                (required as usize - 4) / 2,
            )
        };
        let path = decode_utf16(path).ok_or_else(|| Error::Backend {
            backend: "usbprint discovery",
            message: "invalid UTF-16 interface path".into(),
        })?;
        let (mut vendor_id, mut product_id) = parse_vid_pid(&path);
        let mut node = info.DevInst;
        let mut serial_number = None;
        let mut device_instance_id = None;
        // USBPRINT 子节点通常只暴露型号，父 USB 节点才有真实 VID/PID。
        // 只有带 UNIQUEID 标记的节点，其实例 ID 尾段才作为序列号使用。
        for depth in 0..8 {
            if let Some(id) = instance_id(node) {
                if depth == 0 {
                    device_instance_id = Some(id.clone());
                }
                let (vid, pid) = parse_vid_pid(&id);
                vendor_id = vendor_id.or(vid);
                product_id = product_id.or(pid);
                if vid.is_some() && pid.is_some() && unique_id(node) {
                    serial_number = id.rsplit('\\').next().map(str::to_owned);
                    break;
                }
            }
            let mut parent = 0;
            // SAFETY: 设备节点和输出指针有效，标志按 API 要求设为零。
            if unsafe { CM_Get_Parent(&mut parent, node, 0) } != CR_SUCCESS {
                break;
            }
            node = parent;
        }
        devices.push(DeviceInfo {
            id: DeviceId {
                endpoint: DevicePath::UsbPrint(path),
                generation: 0,
            },
            transport: Transport::UsbPrint,
            bus: Bus::Usb,
            vendor_id,
            product_id,
            serial_number,
            manufacturer: property(set.0, &info, SPDRP_MFG),
            product: property(set.0, &info, SPDRP_FRIENDLYNAME)
                .or_else(|| property(set.0, &info, SPDRP_DEVICEDESC)),
            hid: None,
            usb_print: Some(UsbPrintInfo {
                location: property(set.0, &info, SPDRP_LOCATION_INFORMATION),
                device_instance_id,
            }),
        });
    }
    Ok(devices)
}

fn decode_utf16(value: &[u16]) -> Option<String> {
    let end = value.iter().position(|&v| v == 0)?;
    String::from_utf16(&value[..end]).ok()
}

fn property(
    set: HDEVINFO,
    info: &SP_DEVINFO_DATA,
    property: SETUP_DI_REGISTRY_PROPERTY,
) -> Option<String> {
    let mut buffer = vec![0_u16; 1024];
    // SAFETY: 查询可选字符串属性，将已对齐的 UTF-16 存储作为字节缓冲区传入。
    if unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set,
            info,
            property,
            ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 2) as u32,
            ptr::null_mut(),
        )
    } == 0
    {
        return None;
    }
    decode_utf16(&buffer).filter(|v| !v.is_empty())
}

fn instance_id(node: u32) -> Option<String> {
    let mut size = 0;
    // SAFETY: 设备节点和用于返回大小的指针均有效。
    if unsafe { CM_Get_Device_ID_Size(&mut size, node, 0) } != CR_SUCCESS || size > 65_536 {
        return None;
    }
    let mut buffer = vec![0_u16; size as usize + 1];
    // SAFETY: 缓冲区包含末尾 NUL 所需空间。
    if unsafe { CM_Get_Device_IDW(node, buffer.as_mut_ptr(), buffer.len() as u32, 0) } != CR_SUCCESS
    {
        return None;
    }
    decode_utf16(&buffer)
}

fn unique_id(node: u32) -> bool {
    let mut capabilities = 0_u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: DWORD 属性写入大小匹配的 u32，缓冲区大小参数已初始化。
    (unsafe {
        CM_Get_DevNode_Registry_PropertyW(
            node,
            CM_DRP_CAPABILITIES,
            ptr::null_mut(),
            ptr::from_mut(&mut capabilities).cast(),
            &mut size,
            0,
        ) == CR_SUCCESS
    }) && capabilities & CM_DEVCAP_UNIQUEID != 0
}
