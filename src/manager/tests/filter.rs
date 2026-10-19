use super::*;

#[test]
fn hid_usage_filters_are_exact_optional_and_require_hid_metadata() {
    let serial = device("A");
    let mut printer = serial.clone();
    printer.transport = Transport::UsbPrint;
    printer.id.endpoint = DevicePath::UsbPrint("print-test".into());
    let mut hid = serial.clone();
    hid.transport = Transport::Hid;
    hid.id.endpoint = DevicePath::Hid(b"hid-test".to_vec());
    hid.hid = Some(HidInfo {
        interface_number: Some(0),
        usage_page: 0,
        usage: 0,
    });
    for filter in [
        DeviceFilter {
            usage_page: Some(0),
            ..Default::default()
        },
        DeviceFilter {
            usage: Some(0),
            ..Default::default()
        },
        DeviceFilter {
            usage_page: Some(0),
            usage: Some(0),
            ..Default::default()
        },
    ] {
        assert!(filter.matches(&hid));
        assert!(!filter.matches(&serial));
        assert!(!filter.matches(&printer));
    }
    hid.hid.as_mut().unwrap().usage = 1;
    assert!(
        DeviceFilter {
            usage_page: Some(0),
            ..Default::default()
        }
        .matches(&hid)
    );
    assert!(
        !DeviceFilter {
            usage: Some(0),
            ..Default::default()
        }
        .matches(&hid)
    );
    hid.hid.as_mut().unwrap().usage_page = 1;
    assert!(
        DeviceFilter {
            usage: Some(1),
            ..Default::default()
        }
        .matches(&hid)
    );
    assert!(
        !DeviceFilter {
            usage_page: Some(0),
            ..Default::default()
        }
        .matches(&hid)
    );
}

#[test]
fn multiple_configurations_keep_vid_pid_pairs_together() {
    let filters = DeviceFilter::any_of([
        DeviceFilter {
            vendor_id: Some(0x1234),
            product_id: Some(0x5678),
            ..Default::default()
        },
        DeviceFilter {
            vendor_id: Some(0x36b0),
            product_id: Some(0x5320),
            ..Default::default()
        },
    ]);
    for (vendor_id, product_id, expected) in [
        (Some(0x1234), Some(0x5678), true),
        (Some(0x36b0), Some(0x5320), true),
        (Some(0x1234), Some(0x5320), false),
        (Some(0x36b0), Some(0x5678), false),
        (None, Some(0x5678), false),
        (Some(0x1234), None, false),
    ] {
        let mut candidate = device("A");
        candidate.vendor_id = vendor_id;
        candidate.product_id = product_id;
        assert_eq!(filters.matches(&candidate), expected);
    }
}

#[test]
fn mixed_transport_configurations_keep_all_branch_constraints() {
    let serial = device("A");
    let mut hid = serial.clone();
    hid.transport = Transport::Hid;
    hid.id.endpoint = DevicePath::Hid(b"hid-test".to_vec());
    hid.hid = Some(HidInfo {
        interface_number: Some(0),
        usage_page: 0xff00,
        usage: 1,
    });
    let mut printer = serial.clone();
    printer.transport = Transport::UsbPrint;
    printer.id.endpoint = DevicePath::UsbPrint("print-test".into());
    printer.vendor_id = Some(0x36b0);
    printer.product_id = Some(0x5320);
    let filters = DeviceFilter::any_of([
        DeviceFilter {
            transport: Some(Transport::Serial),
            bus: Some(Bus::Usb),
            serial_number: Some("A".into()),
            path: Some(serial.id.endpoint.clone()),
            ..Default::default()
        },
        DeviceFilter {
            transport: Some(Transport::Hid),
            usage_page: Some(0xff00),
            usage: Some(1),
            ..Default::default()
        },
        DeviceFilter {
            transport: Some(Transport::UsbPrint),
            vendor_id: Some(0x36b0),
            product_id: Some(0x5320),
            ..Default::default()
        },
    ]);
    for candidate in [&serial, &hid, &printer] {
        assert!(filters.matches(candidate));
    }
    let mut wrong_bus = serial.clone();
    wrong_bus.bus = Bus::Bluetooth;
    let mut wrong_serial = serial.clone();
    wrong_serial.serial_number = Some("B".into());
    let mut wrong_path = serial.clone();
    wrong_path.id.endpoint = DevicePath::Serial("COM_OTHER".into());
    let mut wrong_usage_page = hid.clone();
    wrong_usage_page.hid.as_mut().unwrap().usage_page = 1;
    let mut wrong_usage = hid;
    wrong_usage.hid.as_mut().unwrap().usage = 2;
    let mut wrong_transport = printer;
    wrong_transport.transport = Transport::Serial;
    for candidate in [
        wrong_bus,
        wrong_serial,
        wrong_path,
        wrong_usage_page,
        wrong_usage,
        wrong_transport,
    ] {
        assert!(!filters.matches(&candidate), "{candidate:?}");
    }
}

#[test]
fn empty_filter_sets_match_none_and_default_single_filter_matches_all() {
    let candidate = device("A");
    assert!(DeviceFilter::default().matches(&candidate));
    assert!(DeviceFilterSet::from(DeviceFilter::default()).matches(&candidate));
    assert!(!DeviceFilterSet::default().matches(&candidate));
    assert!(!DeviceFilter::any_of([]).matches(&candidate));
    assert!(!DeviceFilterSet::from(Vec::new()).matches(&candidate));
    assert!(!DeviceFilterSet::from([]).matches(&candidate));
    assert!(
        DeviceFilter::any_of([
            DeviceFilter {
                vendor_id: Some(0xffff),
                ..Default::default()
            },
            DeviceFilter::default(),
        ])
        .matches(&candidate)
    );
}

#[test]
fn multiple_configurations_work_with_async_and_blocking_discovery_and_blocking_watch() {
    let mut first = device("A");
    first.id.endpoint = DevicePath::Serial("COM_A".into());
    let mut second = device("B");
    second.id.endpoint = DevicePath::Serial("COM_B".into());
    second.vendor_id = Some(0x36b0);
    second.product_id = Some(0x5320);
    let mut excluded = device("C");
    excluded.id.endpoint = DevicePath::Serial("COM_C".into());
    excluded.product_id = Some(0x5320);
    let devices = vec![first, second, excluded];
    let filters = [
        DeviceFilter {
            vendor_id: Some(0x1234),
            product_id: Some(0x5678),
            ..Default::default()
        },
        DeviceFilter {
            vendor_id: Some(0x36b0),
            product_id: Some(0x5320),
            ..Default::default()
        },
        DeviceFilter {
            vendor_id: Some(0x1234),
            product_id: Some(0x5678),
            ..Default::default()
        },
    ];
    let mut inner = inner(16);
    let (refresh, mut requests) = mpsc::channel::<oneshot::Sender<Result<Vec<DeviceInfo>>>>(1);
    Arc::get_mut(&mut inner).unwrap().refresh = refresh;
    let snapshot = apply_snapshot(&inner, devices);
    let expected = snapshot[..2].to_vec();
    let worker = runtime::handle().unwrap().spawn(async move {
        for _ in 0..3 {
            requests
                .recv()
                .await
                .unwrap()
                .send(Ok(snapshot.clone()))
                .unwrap();
        }
    });
    let manager = DeviceManager { inner };
    assert_eq!(
        manager.discover_blocking(filters.clone()).unwrap(),
        expected
    );
    let async_manager = manager.clone();
    let async_filters = DeviceFilter::any_of(filters.clone());
    assert_eq!(
        runtime::blocking(async move { async_manager.discover(async_filters).await }).unwrap(),
        expected
    );
    assert!(manager.discover_blocking(Vec::new()).unwrap().is_empty());
    let mut watcher = manager.watch_blocking(filters.to_vec());
    assert!(
        matches!(watcher.recv().unwrap(), DeviceEvent::Snapshot { devices, .. }
        if devices == expected)
    );
    let mut empty_watcher = manager.watch_blocking(DeviceFilter::any_of([]));
    assert!(
        matches!(empty_watcher.recv().unwrap(), DeviceEvent::Snapshot { devices, .. }
        if devices.is_empty())
    );
    runtime::blocking(async move { worker.await.map_err(|_| Error::Closed) }).unwrap();
}
