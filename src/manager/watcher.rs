//! 订阅队列与筛选成员变化处理，同步接口复用异步订阅。

use super::*;

/// 异步设备订阅，首个有效设备事件为完整快照；消费过慢时要求重新同步。
pub struct DeviceWatcher {
    pub(super) receiver: broadcast::Receiver<DeviceEvent>,
    pub(super) initial: Option<DeviceEvent>,
    pub(super) filter: DeviceFilterSet,
    pub(super) _manager: DeviceManager,
}

impl DeviceWatcher {
    /// 等待下一条匹配事件；错误和溢出提示始终向调用方传递。
    pub async fn recv(&mut self) -> Result<DeviceEvent> {
        if let Some(initial) = self.initial.take() {
            return Ok(initial);
        }
        loop {
            let event = match self.receiver.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    return Ok(DeviceEvent::ResyncRequired { missed });
                }
                Err(broadcast::error::RecvError::Closed) => return Err(Error::Closed),
            };
            if let Some(event) = filter_event(&self.filter, event) {
                return Ok(event);
            }
        }
    }
}

pub(super) fn filter_event(filter: &DeviceFilterSet, event: DeviceEvent) -> Option<DeviceEvent> {
    match event {
        DeviceEvent::Snapshot { revision, devices } => Some(DeviceEvent::Snapshot {
            revision,
            devices: devices.into_iter().filter(|v| filter.matches(v)).collect(),
        }),
        DeviceEvent::Added { revision, device } => filter
            .matches(&device)
            .then_some(DeviceEvent::Added { revision, device }),
        DeviceEvent::Removed { revision, device } => filter
            .matches(&device)
            .then_some(DeviceEvent::Removed { revision, device }),
        DeviceEvent::Changed {
            revision,
            previous,
            device,
        } => match (filter.matches(&previous), filter.matches(&device)) {
            // 从整个配置并集判断成员关系，跨配置切换仍然属于 Changed。
            (true, true) => Some(DeviceEvent::Changed {
                revision,
                previous,
                device,
            }),
            (true, false) => Some(DeviceEvent::Removed {
                revision,
                device: previous,
            }),
            (false, true) => Some(DeviceEvent::Added { revision, device }),
            (false, false) => None,
        },
        event => Some(event),
    }
}

/// 同步订阅包装，接收队列与筛选逻辑直接复用异步实现。
pub struct BlockingDeviceWatcher {
    pub(super) watcher: Option<DeviceWatcher>,
}

impl BlockingDeviceWatcher {
    pub fn recv(&mut self) -> Result<DeviceEvent> {
        runtime::check_blocking()?;
        let mut watcher = self.watcher.take().ok_or(Error::Closed)?;
        // 即使接收返回错误也还原订阅，避免下一次调用丢失接收器。
        let (watcher, result) = runtime::blocking(async move {
            let result = watcher.recv().await;
            Ok((watcher, result))
        })?;
        self.watcher = Some(watcher);
        result
    }
}
