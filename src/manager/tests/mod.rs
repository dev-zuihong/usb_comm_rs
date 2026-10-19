//! 使用内存快照测试设备状态与筛选，不打开真实外设。

use super::*;
use super::{monitor::apply_snapshot, watcher::filter_event};

mod filter;
mod lifecycle;
mod watcher;

async fn next_event(watcher: &mut DeviceWatcher) -> DeviceEvent {
    tokio::time::timeout(Duration::from_secs(2), watcher.recv())
        .await
        .expect("等待设备事件超时")
        .expect("订阅意外关闭")
}

fn inner(capacity: usize) -> Arc<Inner> {
    let (events, _) = broadcast::channel(capacity);
    let (refresh, _) = mpsc::channel(1);
    Arc::new(Inner {
        snapshot: Mutex::new(Snapshot::default()),
        events,
        refresh,
        sessions: Mutex::new(Vec::new()),
        task: Mutex::new(None),
    })
}

fn device(serial: &str) -> DeviceInfo {
    DeviceInfo {
        id: DeviceId {
            endpoint: DevicePath::Serial("COM_TEST".into()),
            generation: 0,
        },
        transport: Transport::Serial,
        bus: Bus::Usb,
        vendor_id: Some(0x1234),
        product_id: Some(0x5678),
        serial_number: Some(serial.into()),
        manufacturer: None,
        product: None,
        hid: None,
        usb_print: None,
    }
}
