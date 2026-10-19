use std::collections::HashSet;
use usb_comm::{DeviceEvent, DeviceFilter, DeviceManager, ManagerOptions};

/// 只读冒烟测试：注册原生监听并枚举系统 endpoint，不打开任何具体外设。
#[test]
fn native_monitor_and_system_enumeration() {
    let manager = DeviceManager::new(ManagerOptions::default()).unwrap();
    let devices = manager.discover_blocking(DeviceFilter::default()).unwrap();
    let mut endpoints = HashSet::new();
    for device in &devices {
        assert!(device.id.generation > 0);
        assert!(endpoints.insert(device.id.endpoint.clone()));
    }
    let mut watcher = manager.watch_blocking(DeviceFilter::default());
    assert!(
        matches!(watcher.recv().unwrap(), DeviceEvent::Snapshot { revision, .. } if revision > 0)
    );
}
