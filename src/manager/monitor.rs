//! 合并原生通知和定时校准，原子更新快照、连接代次及会话状态。

use super::*;

pub(super) async fn monitor_loop(
    inner: Weak<Inner>,
    options: ManagerOptions,
    _monitor: native::Monitor,
    signal: native::Signal,
    mut requests: mpsc::Receiver<oneshot::Sender<Result<Vec<DeviceInfo>>>>,
) {
    let mut interval = tokio::time::interval(options.reconcile_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut retry = None;
    let mut retries_left = 0_u8;
    loop {
        let mut reply = None;
        tokio::select! {
            request = requests.recv() => {
                match request {
                    Some(request) => reply = Some(request),
                    None => break,
                }
            },
            _ = signal.changed.notified() => {
                if let Some(error) = signal.failure.lock().unwrap().take()
                    && let Some(inner) = inner.upgrade()
                {
                    let _ = inner.events.send(DeviceEvent::BackendError(error));
                }
                tokio::time::sleep(options.debounce).await;
                // 驱动节点可能分阶段就绪：立即枚举后，以 100/300/1000 ms 的间隔补查。
                retries_left = 3;
                retry = Some(tokio::time::Instant::now() + Duration::from_millis(100));
            },
            _ = interval.tick() => {},
            _ = runtime::sleep_until(retry) => {
                retries_left = retries_left.saturating_sub(1);
                retry = match retries_left {
                    2 => Some(tokio::time::Instant::now() + Duration::from_millis(300)),
                    1 => Some(tokio::time::Instant::now() + Duration::from_millis(1000)),
                    _ => None,
                };
            },
        }
        let options = options.clone();
        let result = tokio::task::spawn_blocking(move || discovery::enumerate(&options))
            .await
            .map_err(|e| Error::Backend {
                backend: "discovery",
                message: e.to_string(),
            })
            .and_then(|v| v);
        let Some(inner) = inner.upgrade() else {
            break;
        };
        let result = result.map(|devices| apply_snapshot(&inner, devices));
        if let Err(error) = &result {
            tracing::warn!(%error, "device enumeration failed");
            let _ = inner.events.send(DeviceEvent::BackendError(error.clone()));
        }
        if let Some(reply) = reply {
            let _ = reply.send(result);
        }
    }
}

pub(super) fn apply_snapshot(inner: &Inner, devices: Vec<DeviceInfo>) -> Vec<DeviceInfo> {
    let mut snapshot = inner.snapshot.lock().unwrap();
    snapshot.revision += 1;
    let revision = snapshot.revision;
    let mut next = BTreeMap::new();
    for mut device in devices {
        let path = device.id.endpoint.clone();
        device.id.generation = match snapshot.devices.get(&path) {
            Some(previous) if same_identity(previous, &device) => previous.id.generation,
            _ => {
                snapshot.next_generation += 1;
                snapshot.next_generation
            }
        };
        next.insert(path, device);
    }
    if snapshot.initialized {
        // 先移除旧代次，再添加新代次，保证相同路径替换设备时事件有序。
        for (path, previous) in &snapshot.devices {
            if next.get(path).is_none_or(|v| v.id != previous.id) {
                let mut sessions = inner.sessions.lock().unwrap();
                sessions.retain(|(endpoint, weak)| {
                    if let Some(core) = weak.upgrade() {
                        if endpoint == path {
                            core.disconnect("endpoint removed or replaced");
                        }
                        true
                    } else {
                        false
                    }
                });
                let _ = inner.events.send(DeviceEvent::Removed {
                    revision,
                    device: previous.clone(),
                });
            }
        }
        for (path, device) in &next {
            match snapshot.devices.get(path) {
                Some(previous) if previous.id == device.id => {
                    if previous != device {
                        let _ = inner.events.send(DeviceEvent::Changed {
                            revision,
                            previous: previous.clone(),
                            device: device.clone(),
                        });
                    }
                }
                _ => {
                    let _ = inner.events.send(DeviceEvent::Added {
                        revision,
                        device: device.clone(),
                    });
                }
            }
        }
    } else {
        let _ = inner.events.send(DeviceEvent::Snapshot {
            revision,
            devices: next.values().cloned().collect(),
        });
    }
    snapshot.devices = next;
    snapshot.initialized = true;
    snapshot.devices.values().cloned().collect()
}

fn same_identity(a: &DeviceInfo, b: &DeviceInfo) -> bool {
    // Usage、产品标签等属于可变元数据，变化时保留连接代次并发出 Changed。
    a.transport == b.transport
        && a.vendor_id == b.vendor_id
        && a.product_id == b.product_id
        && a.serial_number == b.serial_number
}
