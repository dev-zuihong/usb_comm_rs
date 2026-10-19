use super::*;

#[tokio::test]
async fn watch_snapshot_filter_and_overflow_are_explicit() {
    let inner = inner(2);
    apply_snapshot(&inner, vec![device("A")]);
    let manager = DeviceManager {
        inner: inner.clone(),
    };
    let mut watcher = manager.watch(DeviceFilter::default());
    assert!(
        matches!(next_event(&mut watcher).await, DeviceEvent::Snapshot { devices, .. } if devices.len() == 1)
    );
    for serial in ["B", "C", "D"] {
        apply_snapshot(&inner, vec![device(serial)]);
    }
    assert!(
        matches!(next_event(&mut watcher).await, DeviceEvent::ResyncRequired { missed } if missed > 0)
    );
    let mut filtered = manager.watch(DeviceFilter {
        transport: Some(Transport::Hid),
        ..Default::default()
    });
    assert!(
        matches!(next_event(&mut filtered).await, DeviceEvent::Snapshot { devices, .. } if devices.is_empty())
    );
}

#[test]
fn filter_membership_change_becomes_remove_or_add() {
    let filter = DeviceFilterSet::from(DeviceFilter {
        vendor_id: Some(0x1234),
        ..Default::default()
    });
    let previous = device("A");
    let mut next = previous.clone();
    next.vendor_id = Some(0x9999);
    assert!(matches!(
        filter_event(
            &filter,
            DeviceEvent::Changed {
                revision: 2,
                previous: previous.clone(),
                device: next.clone()
            }
        ),
        Some(DeviceEvent::Removed { .. })
    ));
    assert!(matches!(
        filter_event(
            &filter,
            DeviceEvent::Changed {
                revision: 3,
                previous: next,
                device: previous
            }
        ),
        Some(DeviceEvent::Added { .. })
    ));
}

#[tokio::test]
async fn composite_hid_interfaces_filter_usage_pairs_in_snapshots_and_hotplug() {
    let endpoint = |name: &str, usage_page, usage| {
        let mut hid = device("COMPOSITE");
        hid.transport = Transport::Hid;
        hid.id.endpoint = DevicePath::Hid(name.as_bytes().to_vec());
        hid.hid = Some(HidInfo {
            interface_number: Some(0),
            usage_page,
            usage,
        });
        hid
    };
    // 所有 endpoint 的 VID/PID/序列号相同，只有路径和 Usage 配对不同。
    let target = endpoint("collection-a", 0xff00, 1);
    let other_usage = endpoint("collection-b", 0xff00, 2);
    let other_page = endpoint("collection-c", 0xff01, 1);
    let alternative = endpoint("collection-d", 0xff01, 2);
    let filter = DeviceFilter {
        transport: Some(Transport::Hid),
        vendor_id: Some(0x1234),
        product_id: Some(0x5678),
        usage_page: Some(0xff00),
        usage: Some(1),
        ..Default::default()
    };
    let filters = DeviceFilter::any_of([
        filter.clone(),
        DeviceFilter {
            usage_page: Some(0xff01),
            usage: Some(2),
            ..filter.clone()
        },
    ]);
    let inner = inner(16);
    apply_snapshot(
        &inner,
        vec![
            target.clone(),
            other_usage.clone(),
            other_page.clone(),
            alternative.clone(),
        ],
    );
    let manager = DeviceManager {
        inner: inner.clone(),
    };
    let mut single = manager.watch(filter);
    let mut multiple = manager.watch(filters);
    assert!(
        matches!(next_event(&mut single).await, DeviceEvent::Snapshot { devices, .. }
        if devices.len() == 1 && devices[0].id.endpoint == target.id.endpoint)
    );
    assert!(
        matches!(next_event(&mut multiple).await, DeviceEvent::Snapshot { devices, .. }
        if devices.len() == 2 && devices[0].id.endpoint == target.id.endpoint
            && devices[1].id.endpoint == alternative.id.endpoint)
    );

    // 接收时须跳过不匹配的 Added，再返回匹配的 Removed。
    let extra = endpoint("collection-e", 0xff00, 2);
    apply_snapshot(
        &inner,
        vec![
            target.clone(),
            other_usage.clone(),
            other_page.clone(),
            alternative.clone(),
            extra.clone(),
        ],
    );
    let mut remaining = vec![other_usage, other_page, alternative, extra];
    apply_snapshot(&inner, remaining.clone());
    for watcher in [&mut single, &mut multiple] {
        let event = next_event(watcher).await;
        assert!(matches!(event, DeviceEvent::Removed { device, .. }
            if device.id.endpoint == target.id.endpoint));
        assert!(watcher.receiver.is_empty());
    }
    remaining.push(target.clone());
    apply_snapshot(&inner, remaining);
    for watcher in [&mut single, &mut multiple] {
        let event = next_event(watcher).await;
        assert!(matches!(event, DeviceEvent::Added { device, .. }
            if device.id.endpoint == target.id.endpoint));
        assert!(watcher.receiver.is_empty());
    }
}

#[tokio::test]
async fn overlapping_configurations_do_not_duplicate_snapshots_or_events() {
    for initialized in [false, true] {
        let inner = inner(16);
        let included = device("A");
        let mut excluded = device("B");
        excluded.id.endpoint = DevicePath::Serial("COM_OTHER".into());
        excluded.vendor_id = Some(0xffff);
        if initialized {
            apply_snapshot(&inner, vec![included.clone(), excluded.clone()]);
        }
        let manager = DeviceManager {
            inner: inner.clone(),
        };
        let mut watcher = manager.watch(vec![
            DeviceFilter {
                vendor_id: Some(0x1234),
                ..Default::default()
            },
            DeviceFilter {
                serial_number: Some("A".into()),
                ..Default::default()
            },
        ]);
        if !initialized {
            apply_snapshot(&inner, vec![included.clone(), excluded.clone()]);
        }
        let event = next_event(&mut watcher).await;
        assert!(matches!(event, DeviceEvent::Snapshot { devices, .. }
                if devices.len() == 1 && devices[0].serial_number.as_deref() == Some("A")));
        apply_snapshot(&inner, vec![excluded.clone()]);
        let event = next_event(&mut watcher).await;
        assert!(matches!(event, DeviceEvent::Removed { .. }));
        apply_snapshot(&inner, vec![included.clone(), excluded.clone()]);
        let event = next_event(&mut watcher).await;
        assert!(matches!(event, DeviceEvent::Added { .. }));
        let mut updated = included;
        updated.product = Some("updated label".into());
        apply_snapshot(&inner, vec![updated, excluded]);
        let event = next_event(&mut watcher).await;
        assert!(matches!(event, DeviceEvent::Changed { .. }));
        assert!(watcher.receiver.is_empty());
    }
}

#[tokio::test]
async fn moving_between_configurations_preserves_union_membership() {
    let inner = inner(16);
    let mut hid = device("A");
    hid.transport = Transport::Hid;
    hid.id.endpoint = DevicePath::Hid(b"hid-test".to_vec());
    hid.hid = Some(HidInfo {
        interface_number: Some(0),
        usage_page: 0xff00,
        usage: 1,
    });
    apply_snapshot(&inner, vec![hid.clone()]);
    let manager = DeviceManager {
        inner: inner.clone(),
    };
    let mut watcher = manager.watch(DeviceFilter::any_of([1, 2].map(|usage| DeviceFilter {
        usage: Some(usage),
        ..Default::default()
    })));
    assert!(matches!(
        next_event(&mut watcher).await,
        DeviceEvent::Snapshot { .. }
    ));
    for usage in [2, 3, 1] {
        hid.hid.as_mut().unwrap().usage = usage;
        apply_snapshot(&inner, vec![hid.clone()]);
        let event = next_event(&mut watcher).await;
        match usage {
            2 => assert!(
                matches!(event, DeviceEvent::Changed { previous, device, .. }
                if previous.id == device.id
                    && previous.hid.as_ref().unwrap().usage == 1
                    && device.hid.as_ref().unwrap().usage == 2)
            ),
            3 => assert!(matches!(event, DeviceEvent::Removed { device, .. }
                if device.hid.as_ref().unwrap().usage == 2)),
            1 => assert!(matches!(event, DeviceEvent::Added { device, .. }
                if device.hid.as_ref().unwrap().usage == 1)),
            _ => unreachable!(),
        }
        if usage == 3 {
            // 变更前后均不匹配，应跳过此事件，直到之后的 Added。
            hid.hid.as_mut().unwrap().usage = 4;
            apply_snapshot(&inner, vec![hid.clone()]);
        }
    }
    assert!(watcher.receiver.is_empty());
}
