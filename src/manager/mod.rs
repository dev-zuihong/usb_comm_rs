//! 设备管理入口；后台枚举和订阅分别由 monitor、watcher 维护。

use crate::{discovery, native, runtime, session, *};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::{broadcast, mpsc, oneshot};

mod monitor;
#[cfg(test)]
mod tests;
mod watcher;

use monitor::monitor_loop;
pub use watcher::{BlockingDeviceWatcher, DeviceWatcher};

#[derive(Default)]
struct Snapshot {
    revision: u64,
    next_generation: u64,
    devices: BTreeMap<DevicePath, DeviceInfo>,
    initialized: bool,
}

struct Inner {
    snapshot: Mutex<Snapshot>,
    events: broadcast::Sender<DeviceEvent>,
    refresh: mpsc::Sender<oneshot::Sender<Result<Vec<DeviceInfo>>>>,
    sessions: Mutex<Vec<(DevicePath, Weak<session::Core>)>>,
    task: Mutex<Option<tokio::task::AbortHandle>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().unwrap().take() {
            task.abort();
        }
    }
}

/// 可克隆的设备管理器；最后一个管理器和订阅释放后停止发现与监听。
/// 已有会话保持有效，由各自的 I/O 继续检测连接错误。
#[derive(Clone)]
pub struct DeviceManager {
    inner: Arc<Inner>,
}

impl DeviceManager {
    pub fn new(options: ManagerOptions) -> Result<Self> {
        options.validate()?;
        let handle = runtime::handle()?;
        // 先完成原生监听注册，再首次枚举，避免启动期间漏掉设备变化。
        let (monitor, signal) = native::Monitor::start()?;
        let (events, _) = broadcast::channel(options.event_capacity);
        let (refresh, requests) = mpsc::channel(16);
        let inner = Arc::new(Inner {
            snapshot: Mutex::new(Snapshot::default()),
            events,
            refresh,
            sessions: Mutex::new(Vec::new()),
            task: Mutex::new(None),
        });
        let task = handle.spawn(monitor_loop(
            Arc::downgrade(&inner),
            options,
            monitor,
            signal,
            requests,
        ));
        *inner.task.lock().unwrap() = Some(task.abort_handle());
        Ok(Self { inner })
    }

    /// 重新枚举并返回筛选后的快照；枚举失败时保留上次列表。
    /// 支持单组配置、Vec/数组和 [`DeviceFilter::any_of`]。
    pub async fn discover(&self, filter: impl Into<DeviceFilterSet>) -> Result<Vec<DeviceInfo>> {
        let filter = filter.into();
        let inner = self.inner.clone();
        runtime::run(async move {
            let (reply, response) = oneshot::channel();
            inner.refresh.send(reply).await.map_err(|_| Error::Closed)?;
            Ok(response
                .await
                .map_err(|_| Error::Closed)??
                .into_iter()
                .filter(|v| filter.matches(v))
                .collect())
        })
        .await
    }

    pub fn discover_blocking(&self, filter: impl Into<DeviceFilterSet>) -> Result<Vec<DeviceInfo>> {
        runtime::check_blocking()?;
        let filter = filter.into();
        let manager = self.clone();
        runtime::blocking(async move { manager.discover(filter).await })
    }

    /// 原子订阅并取得缓存快照；启动尚未完成时，首次成功枚举会发出 Snapshot。
    /// 筛选参数与 [`Self::discover`] 相同，支持单组及多组配置。
    pub fn watch(&self, filter: impl Into<DeviceFilterSet>) -> DeviceWatcher {
        let filter = filter.into();
        let snapshot = self.inner.snapshot.lock().unwrap();
        let receiver = self.inner.events.subscribe();
        let initial = snapshot.initialized.then(|| DeviceEvent::Snapshot {
            revision: snapshot.revision,
            devices: snapshot
                .devices
                .values()
                .filter(|v| filter.matches(v))
                .cloned()
                .collect(),
        });
        DeviceWatcher {
            receiver,
            initial,
            filter,
            _manager: self.clone(),
        }
    }

    pub fn watch_blocking(&self, filter: impl Into<DeviceFilterSet>) -> BlockingDeviceWatcher {
        BlockingDeviceWatcher {
            watcher: Some(self.watch(filter)),
        }
    }

    pub async fn open_serial(
        &self,
        device: &DeviceInfo,
        config: SerialConfig,
        io: IoOptions,
    ) -> Result<SerialSession> {
        self.ensure_current(device)?;
        let DevicePath::Serial(path) = &device.id.endpoint else {
            return Err(Error::InvalidConfig("expected a serial endpoint".into()));
        };
        let session = session::open_serial(path.clone(), config, io).await?;
        self.register_checked(device, &session.core)?;
        Ok(session)
    }

    pub async fn open_serial_path(
        &self,
        path: impl Into<String>,
        config: SerialConfig,
        io: IoOptions,
    ) -> Result<SerialSession> {
        let path = path.into();
        let session = session::open_serial(path.clone(), config, io).await?;
        let mut sessions = self.inner.sessions.lock().unwrap();
        sessions.retain(|(_, weak)| weak.strong_count() > 0);
        sessions.push((DevicePath::Serial(path), Arc::downgrade(&session.core)));
        Ok(session)
    }

    pub async fn open_hid(&self, device: &DeviceInfo, options: HidOptions) -> Result<HidSession> {
        self.ensure_current(device)?;
        let DevicePath::Hid(path) = &device.id.endpoint else {
            return Err(Error::InvalidConfig("expected a HID endpoint".into()));
        };
        let session = session::open_hid(path.clone(), options).await?;
        self.register_checked(device, &session.core)?;
        Ok(session)
    }

    pub async fn open_usb_print(
        &self,
        device: &DeviceInfo,
        options: UsbPrintOptions,
    ) -> Result<UsbPrintSession> {
        if !cfg!(windows) {
            return Err(Error::Unsupported(
                "USB Print uses the Windows usbprint.sys driver".into(),
            ));
        }
        self.ensure_current(device)?;
        let DevicePath::UsbPrint(path) = &device.id.endpoint else {
            return Err(Error::InvalidConfig("expected a USB Print endpoint".into()));
        };
        let session = session::open_usb_print(path.clone(), options).await?;
        self.register_checked(device, &session.core)?;
        Ok(session)
    }

    pub fn open_usb_print_blocking(
        &self,
        device: &DeviceInfo,
        options: UsbPrintOptions,
    ) -> Result<BlockingUsbPrintSession> {
        runtime::check_blocking()?;
        let (manager, device) = (self.clone(), device.clone());
        runtime::blocking(async move {
            Ok(manager
                .open_usb_print(&device, options)
                .await?
                .into_blocking())
        })
    }

    pub fn open_serial_blocking(
        &self,
        device: &DeviceInfo,
        config: SerialConfig,
        io: IoOptions,
    ) -> Result<BlockingSerialSession> {
        runtime::check_blocking()?;
        let (manager, device) = (self.clone(), device.clone());
        runtime::blocking(async move {
            Ok(manager
                .open_serial(&device, config, io)
                .await?
                .into_blocking())
        })
    }

    pub fn open_serial_path_blocking(
        &self,
        path: impl Into<String>,
        config: SerialConfig,
        io: IoOptions,
    ) -> Result<BlockingSerialSession> {
        runtime::check_blocking()?;
        let (manager, path) = (self.clone(), path.into());
        runtime::blocking(async move {
            Ok(manager
                .open_serial_path(path, config, io)
                .await?
                .into_blocking())
        })
    }

    pub fn open_hid_blocking(
        &self,
        device: &DeviceInfo,
        options: HidOptions,
    ) -> Result<BlockingHidSession> {
        runtime::check_blocking()?;
        let (manager, device) = (self.clone(), device.clone());
        runtime::blocking(
            async move { Ok(manager.open_hid(&device, options).await?.into_blocking()) },
        )
    }

    fn ensure_current(&self, device: &DeviceInfo) -> Result<()> {
        let snapshot = self.inner.snapshot.lock().unwrap();
        if snapshot
            .devices
            .get(&device.id.endpoint)
            .is_some_and(|v| v.id == device.id)
        {
            Ok(())
        } else {
            Err(Error::Disconnected(
                "stale device identifier; discover again".into(),
            ))
        }
    }

    fn register_checked(&self, device: &DeviceInfo, core: &Arc<session::Core>) -> Result<()> {
        // 与快照更新保持同一加锁顺序：先快照、后会话表。
        // 代次校验与注册不可被移除事件插入，避免新打开的会话漏掉断开通知。
        let snapshot = self.inner.snapshot.lock().unwrap();
        if snapshot
            .devices
            .get(&device.id.endpoint)
            .is_none_or(|v| v.id != device.id)
        {
            core.disconnect("device changed while opening");
            return Err(Error::Disconnected("device changed while opening".into()));
        }
        let mut sessions = self.inner.sessions.lock().unwrap();
        sessions.retain(|(_, weak)| weak.strong_count() > 0);
        sessions.push((device.id.endpoint.clone(), Arc::downgrade(core)));
        Ok(())
    }
}
