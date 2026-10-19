//! 合并系统枚举结果，按可打开路径保留独立 endpoint。

mod usb_print;

use crate::*;
use std::collections::BTreeMap;

pub(crate) fn enumerate(options: &ManagerOptions) -> Result<Vec<DeviceInfo>> {
    let api = hidapi::HidApi::new()?;
    let mut devices = BTreeMap::new();
    for hid in api.device_list() {
        let endpoint = DevicePath::Hid(hid.path().to_bytes().to_vec());
        devices
            .entry(endpoint.clone())
            .or_insert_with(|| DeviceInfo {
                id: DeviceId {
                    endpoint,
                    generation: 0,
                },
                transport: Transport::Hid,
                bus: match hid.bus_type() {
                    hidapi::BusType::Usb => Bus::Usb,
                    hidapi::BusType::Bluetooth => Bus::Bluetooth,
                    hidapi::BusType::Unknown => Bus::Unknown,
                    _ => Bus::Other,
                },
                vendor_id: Some(hid.vendor_id()),
                product_id: Some(hid.product_id()),
                serial_number: hid.serial_number().map(str::to_owned),
                manufacturer: hid.manufacturer_string().map(str::to_owned),
                product: hid.product_string().map(str::to_owned),
                hid: Some(HidInfo {
                    interface_number: (hid.interface_number() >= 0)
                        .then_some(hid.interface_number()),
                    usage_page: hid.usage_page(),
                    usage: hid.usage(),
                }),
                usb_print: None,
            });
    }
    for port in serialport::available_ports()? {
        if cfg!(target_os = "macos")
            && !options.include_macos_dialin
            && port.port_name.starts_with("/dev/tty.")
        {
            continue;
        }
        let endpoint = DevicePath::Serial(port.port_name);
        let mut device = DeviceInfo {
            id: DeviceId {
                endpoint: endpoint.clone(),
                generation: 0,
            },
            transport: Transport::Serial,
            bus: Bus::Unknown,
            vendor_id: None,
            product_id: None,
            serial_number: None,
            manufacturer: None,
            product: None,
            hid: None,
            usb_print: None,
        };
        match port.port_type {
            serialport::SerialPortType::UsbPort(usb) => {
                device.bus = Bus::Usb;
                device.vendor_id = Some(usb.vid);
                device.product_id = Some(usb.pid);
                device.serial_number = usb.serial_number;
                device.manufacturer = usb.manufacturer;
                device.product = usb.product;
            }
            serialport::SerialPortType::BluetoothPort => device.bus = Bus::Bluetooth,
            serialport::SerialPortType::PciPort => device.bus = Bus::Other,
            _ => {}
        }
        devices.insert(endpoint, device);
    }
    for device in usb_print::enumerate()? {
        devices.insert(device.id.endpoint.clone(), device);
    }
    Ok(devices.into_values().collect())
}
