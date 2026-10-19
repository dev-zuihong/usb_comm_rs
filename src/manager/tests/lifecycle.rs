use super::*;

#[test]
fn removal_and_replug_replace_generation_and_reject_stale_open() {
    let inner = inner(16);
    let old = apply_snapshot(&inner, vec![device("A")]).remove(0);
    let unchanged = apply_snapshot(&inner, vec![device("A")]).remove(0);
    assert_eq!(old.id, unchanged.id);
    apply_snapshot(&inner, vec![]);
    let new = apply_snapshot(&inner, vec![device("A")]).remove(0);
    assert_ne!(new.id, old.id);
    let manager = DeviceManager { inner };
    assert!(matches!(
        manager.ensure_current(&old),
        Err(Error::Disconnected(_))
    ));
    manager.ensure_current(&new).unwrap();
}

#[test]
fn replacement_at_same_path_emits_remove_then_add() {
    let inner = inner(16);
    apply_snapshot(&inner, vec![device("A")]);
    let mut events = inner.events.subscribe();
    let next = apply_snapshot(&inner, vec![device("B")]).remove(0);
    assert!(
        matches!(events.try_recv().unwrap(), DeviceEvent::Removed { device, .. } if device.serial_number.as_deref() == Some("A"))
    );
    assert!(
        matches!(events.try_recv().unwrap(), DeviceEvent::Added { device, .. } if device.id == next.id)
    );
}

#[test]
fn metadata_update_preserves_generation() {
    let inner = inner(16);
    let first = apply_snapshot(&inner, vec![device("A")]).remove(0);
    let mut events = inner.events.subscribe();
    let mut updated = device("A");
    updated.product = Some("updated label".into());
    let next = apply_snapshot(&inner, vec![updated]).remove(0);
    assert_eq!(first.id, next.id);
    assert!(matches!(
        events.try_recv().unwrap(),
        DeviceEvent::Changed { .. }
    ));
}

#[test]
fn usb_print_filters_events_and_replug_use_the_shared_lifecycle() {
    let inner = inner(16);
    let mut printer = device("PTB_TEST");
    printer.transport = Transport::UsbPrint;
    printer.id.endpoint = DevicePath::UsbPrint(r"\\?\usb#vid_36b0&pid_5320#ptb".into());
    printer.vendor_id = Some(0x36b0);
    printer.product_id = Some(0x5320);
    printer.usb_print = Some(UsbPrintInfo {
        location: Some("Port_#0001.Hub_#0002".into()),
        device_instance_id: None,
    });
    let filter = DeviceFilter {
        transport: Some(Transport::UsbPrint),
        vendor_id: Some(0x36b0),
        product_id: Some(0x5320),
        ..Default::default()
    };
    assert!(filter.matches(&printer));
    assert!(
        !DeviceFilter {
            transport: Some(Transport::Serial),
            ..Default::default()
        }
        .matches(&printer)
    );
    let original = apply_snapshot(&inner, vec![printer.clone()]).remove(0);
    let mut events = inner.events.subscribe();
    apply_snapshot(&inner, vec![]);
    assert!(
        matches!(events.try_recv().unwrap(), DeviceEvent::Removed { device, .. } if device.transport == Transport::UsbPrint)
    );
    let replugged = apply_snapshot(&inner, vec![printer]).remove(0);
    assert_ne!(original.id, replugged.id);
    let manager = DeviceManager { inner };
    assert!(matches!(
        manager.ensure_current(&original),
        Err(Error::Disconnected(_))
    ));
}

#[cfg(not(windows))]
#[tokio::test]
async fn usb_print_public_open_reports_platform_support_before_discovery() {
    let manager = DeviceManager { inner: inner(1) };
    let mut printer = device("PTB_TEST");
    printer.transport = Transport::UsbPrint;
    printer.id.endpoint = DevicePath::UsbPrint("test".into());
    assert!(matches!(
        manager
            .open_usb_print(&printer, UsbPrintOptions::default())
            .await,
        Err(Error::Unsupported(_))
    ));
}
