//! 设备筛选：配置内 AND，多配置间 OR；同一 endpoint 只产生一次结果。

use crate::{Bus, DeviceInfo, DevicePath, Transport};

/// 单组筛选配置：所有已设置字段按 AND 匹配，None 表示不限制。
/// 默认配置匹配全部设备。
#[derive(Debug, Clone, Default)]
pub struct DeviceFilter {
    pub transport: Option<Transport>,
    pub bus: Option<Bus>,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub serial_number: Option<String>,
    pub path: Option<DevicePath>,
    /// 精确匹配 HID 顶层集合的 Usage Page；None 不限制，Some(0) 匹配零值。
    /// 没有 HID 元数据的设备不会命中此条件。
    pub usage_page: Option<u16>,
    /// 精确匹配 HID 顶层集合的 Usage；与 usage_page 同时设置时按 AND 匹配。
    /// None 不限制，Some(0) 匹配零值；这不是 Report ID，无 HID 元数据时不匹配。
    pub usage: Option<u16>,
}

impl DeviceFilter {
    /// 组合多组配置，组间按 OR 匹配，保留每组 VID/PID 和 Usage 配对。
    /// 空集合不匹配设备；集合内含默认配置时匹配全部设备。
    pub fn any_of(filters: impl IntoIterator<Item = Self>) -> DeviceFilterSet {
        DeviceFilterSet {
            filters: filters.into_iter().collect(),
        }
    }

    /// 检查单个 endpoint 是否满足本组的全部条件。
    pub fn matches(&self, device: &DeviceInfo) -> bool {
        self.transport.is_none_or(|v| v == device.transport)
            && self.bus.is_none_or(|v| v == device.bus)
            && self.vendor_id.is_none_or(|v| Some(v) == device.vendor_id)
            && self.product_id.is_none_or(|v| Some(v) == device.product_id)
            && self
                .serial_number
                .as_ref()
                .is_none_or(|v| Some(v) == device.serial_number.as_ref())
            && self.path.as_ref().is_none_or(|v| v == &device.id.endpoint)
            && self
                .usage_page
                .is_none_or(|v| device.hid.as_ref().is_some_and(|h| h.usage_page == v))
            && self
                .usage
                .is_none_or(|v| device.hid.as_ref().is_some_and(|h| h.usage == v))
    }
}

/// 多组筛选配置的并集，可通过 [`DeviceFilter::any_of`] 创建，
/// 也可从单个 [`DeviceFilter`]、Vec 或数组转换；重复命中不会重复返回设备或事件。
///
/// 空集合（含默认值）不匹配设备；匹配全部设备请使用 `DeviceFilter::default()`。
#[derive(Debug, Clone, Default)]
pub struct DeviceFilterSet {
    filters: Vec<DeviceFilter>,
}

impl DeviceFilterSet {
    /// 任意一组命中即可；短路求值，空集合返回 false。
    pub fn matches(&self, device: &DeviceInfo) -> bool {
        self.filters.iter().any(|filter| filter.matches(device))
    }
}

impl From<DeviceFilter> for DeviceFilterSet {
    fn from(filter: DeviceFilter) -> Self {
        Self {
            filters: vec![filter],
        }
    }
}

impl From<Vec<DeviceFilter>> for DeviceFilterSet {
    fn from(filters: Vec<DeviceFilter>) -> Self {
        Self { filters }
    }
}

impl<const N: usize> From<[DeviceFilter; N]> for DeviceFilterSet {
    fn from(filters: [DeviceFilter; N]) -> Self {
        Self {
            filters: Vec::from(filters),
        }
    }
}
