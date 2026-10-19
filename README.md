# usb-comm

跨 Windows、Linux、macOS 的 Rust HID / SERIAL 通信库，并支持 Windows USB Print 设备。提供设备发现、原生热插拔监听、打开/关闭、配置和原始数据收发，以及共享实现的同步/异步 API。

采用 `hidapi`、`serialport`、`tokio-serial` 和 Tokio。串口使用异步 I/O；HID 使用专用线程；Windows USB Print 使用每会话一个工作线程，同时维护独立的重叠读写请求。所有管理器共享后台运行时，调用异步接口不要求调用方自行创建 Tokio 运行时。

## 使用

项目尚未发布到 crates.io，可通过路径依赖引入：

```toml
[dependencies]
usb-comm = { path = "../usb" }
```

异步串口：

```no_run
use std::time::Duration;
use usb_comm::{DeviceManager, ManagerOptions, SerialConfig, IoOptions};

# async fn example() -> usb_comm::Result<()> {
let manager = DeviceManager::new(ManagerOptions::default())?;
let port = manager.open_serial_path(
    "COM3", // Linux: /dev/ttyUSB0; macOS: /dev/cu.usbserial-...
    SerialConfig::default(),
    IoOptions::default(),
).await?;
port.write_all(b"hello").await?;
let response = port.recv(Some(Duration::from_secs(2))).await;
port.close().await?;
println!("{:02x?}", response?);
# Ok(())
# }
```

同步调用使用 `open_serial_path_blocking`，返回的会话提供同名阻塞方法。也可通过 `into_blocking` / `into_async` 转换；两种接口共享资源、状态和错误。阻塞方法在 Tokio 上下文中返回 `Error::InvalidContext`，应改用异步方法或在 `spawn_blocking` 中调用。

发现与热插拔：

```no_run
use usb_comm::{DeviceManager, ManagerOptions, DeviceFilter, DeviceEvent, Transport};

# async fn example() -> usb_comm::Result<()> {
let manager = DeviceManager::new(ManagerOptions::default())?;
let filter = DeviceFilter { transport: Some(Transport::Serial), ..Default::default() };
let devices = manager.discover(filter.clone()).await?;
let mut watcher = manager.watch(filter.clone());
loop {
    match watcher.recv().await? {
        DeviceEvent::ResyncRequired { .. } => {
            // Discard the old subscription and resume with an atomic snapshot.
            manager.discover(filter.clone()).await?;
            watcher = manager.watch(filter.clone());
        }
        event => println!("{event:?}"),
    }
}
# }
```

`watch` 的第一条有效设备事件为 `Snapshot`；初始化枚举失败时可能先收到 `BackendError`。枚举返回空列表属于成功，枚举失败则保留上次快照。每批事件携带递增 revision；订阅队列溢出返回 `ResyncRequired`。

多个筛选配置可用 `DeviceFilter::any_of` 组合，同时发现和监听不同型号、不同传输类型的设备：

```no_run
use usb_comm::{DeviceFilter, DeviceManager, ManagerOptions, Transport};

# async fn example() -> usb_comm::Result<()> {
let manager = DeviceManager::new(ManagerOptions::default())?;
let filter = DeviceFilter::any_of([
    DeviceFilter {
        transport: Some(Transport::Hid),
        vendor_id: Some(0x1234), product_id: Some(0x5678),
        usage_page: Some(0xff00),
        usage: Some(0x0001),
        ..Default::default()
    },
    DeviceFilter {
        transport: Some(Transport::Serial),
        vendor_id: Some(0x1a86), product_id: Some(0x7523),
        ..Default::default()
    },
    DeviceFilter {
        transport: Some(Transport::UsbPrint),
        vendor_id: Some(0x36b0), product_id: Some(0x5320),
        ..Default::default()
    },
]);
let devices = manager.discover(filter.clone()).await?;
let mut watcher = manager.watch(filter);
println!("{devices:#?}");
println!("{:?}", watcher.recv().await?);
# Ok(())
# }
```

配置之间为 **OR**，每组内部已设置的字段为 **AND**。例如两个 VID/PID 配置只匹配各自的配对，不会把第一组 VID 和第二组 PID 交叉匹配；命中多组时同一 endpoint 和事件只返回一次。`any_of` 返回可克隆的 `DeviceFilterSet`。`discover` / `watch` 及其阻塞版本也可直接接收 `Vec<DeviceFilter>` 或配置数组，原有单个 `DeviceFilter` 用法保持可用。

空配置集合（如 `DeviceFilter::any_of([])`、空 `Vec`、`DeviceFilterSet::default()`）不匹配任何设备；`DeviceFilter::default()` 匹配所有设备，将它放进配置集合也会匹配所有设备。订阅持有创建时的配置，修改筛选范围需重新调用 `watch`。

复合设备的多个 HID 接口可能共用 VID/PID，可增加 `usage_page`（Usage Page）和 `usage`（Usage）筛选顶层集合。这两个值来自 `hidapi` 的枚举元数据，与 VID/PID 等条件按 AND 匹配；`None` 不限制该字段，`Some(0)` 按零精确匹配。设置任一条件时，没有 HID 元数据的 SERIAL / USB Print 接口不会匹配。多配置中每组可指定各自的 Usage Page/Usage 配对。`usage` 不是 Report ID；若仍有多个匹配接口，可继续用路径或序列号选择。

HID 使用 `discover` 筛选设备后调用 `open_hid`。报告接口显式分离 Report ID 和 payload：

```no_run
use usb_comm::{DeviceManager, ManagerOptions, DeviceFilter, Transport, HidOptions};

# async fn example() -> usb_comm::Result<()> {
let manager = DeviceManager::new(ManagerOptions::default())?;
let devices = manager.discover(DeviceFilter {
    transport: Some(Transport::Hid), vendor_id: Some(0x1234), product_id: Some(0x5678),
    usage_page: Some(0xff00), usage: Some(0x0001),
    ..Default::default()
}).await?;
// Multiple interfaces can share VID/PID. Apply path, usage or serial filters
// rather than opening an arbitrary first match.
if devices.len() != 1 { return Err(usb_comm::Error::InvalidConfig("select one HID endpoint".into())); }
let hid = manager.open_hid(&devices[0], HidOptions {
    numbered_input_reports: true, // Set from the device's report descriptor.
    ..Default::default()
}).await?;
hid.send_report(1, &[0x10, 0x20]).await?;
hid.send_feature_report(2, &[0x30]).await?;
let feature = hid.get_feature_report(2, 32).await?; // payload capacity excludes ID
println!("{feature:?}");
hid.close().await?;
# Ok(())
# }
```

## USB Print / ING PTB

对齐 `ING_PTB/07-gui` 的 USB Print 传输：通过 `usbprint.sys` 的设备接口 GUID `28D78FAD-5A12-11D1-AE5B-0000F803A8C2` 枚举和直接读写，不经过系统打印队列，也不要求改为 WinUSB 驱动。

```no_run
use usb_comm::{DeviceFilter, DeviceManager, ManagerOptions, Transport, UsbPrintOptions};

# async fn example(request: &[u8]) -> usb_comm::Result<()> {
let manager = DeviceManager::new(ManagerOptions::default())?;
let devices = manager.discover(DeviceFilter {
    transport: Some(Transport::UsbPrint),
    vendor_id: Some(0x36b0), product_id: Some(0x5320),
    ..Default::default()
}).await?;
if devices.len() != 1 {
    return Err(usb_comm::Error::InvalidConfig("select one PTB interface path".into()));
}
let usb = manager.open_usb_print(&devices[0], UsbPrintOptions::default()).await?;
// request is a complete application frame, encoded by the caller.
usb.write_all(request).await?;
let response = usb.recv(Some(std::time::Duration::from_secs(2))).await;
usb.close().await?;
println!("{:02x?}", response?);
# Ok(())
# }
```

- `Transport::UsbPrint` / `DevicePath::UsbPrint` 接入同一套发现、筛选、设备代次和热插拔事件。默认枚举所有 USB Print 接口，PTB 示例按 `36B0:5320` 筛选。`DeviceInfo.usb_print` 提供位置和设备实例 ID；路径不含 VID/PID 时查询父节点，实例 ID 只有在系统标记唯一时才被视为序列号。
- `UsbPrintSession` 提供 `write_all`、`recv`、`close`、状态查询及 `into_blocking`。管理器另有 `open_usb_print_blocking`；它不暴露串口波特率、DTR/RTS 或 HID Report 配置。
- 默认 Bulk OUT 最大包为 **64 字节**。每次 `write_all` 完成非空载荷后，若总长度整除最大包大小，就额外执行一次带有效 `OVERLAPPED` 的零字节 `WriteFile`。例如 64、128 字节补 ZLP；0、63、65 字节不补。发送完成通知包含 ZLP 的完成，避免 ZLP 与下一条发送交错。
- 其他设备可设置 `bulk_max_packet_size` 或关闭 `send_zero_length_packet`。包大小不会自动读取 USB 描述符；默认值来自参考 PTB 实现。
- 读、写使用独立的重叠句柄，写入带 `FILE_FLAG_WRITE_THROUGH`。库直接提交数据，无 `FileStream` 缓冲，无需额外刷新托管缓冲；空写入不会自动变成 ZLP。取消和关闭调用 `CancelIoEx`，并等待内核请求结束后释放缓冲区和句柄。
- **仅 Windows 有此后端**。Linux/macOS 发现不返回 `UsbPrint`；尝试打开返回 `Error::Unsupported`，HID/SERIAL 继续按原接口运行。库不自动发送版本查询、下载固件或解析 PTB 帧/CRC。

## 行为约定

- **发现粒度：** 每个可打开的 HID 路径、串口或 USB Print 接口是一个 endpoint。复合设备有多个条目；不根据 VID/PID 合并。HID 路径保留原始字节，不要求 UTF-8。当前版本不提供跨平台物理设备关联 ID。
- **设备标识：** `DeviceId` 包含路径和管理器内的连接代次；它只在同一个管理器中有效。观察到移除再出现或设备身份改变后产生新代次，旧标识无法打开。极短的拔插如果没有被枚举观察到，不能保证区分；不会承诺跨重启稳定标识。
- **发现范围：** 全部系统串口，包括 USB、板载、蓝牙和虚拟串口；HID 也可包含蓝牙。USB 元数据不齐全时使用 `None` / `Unknown`。macOS 默认保留 `/dev/cu.*`、省略 `/dev/tty.*`，可用 `include_macos_dialin` 改变。
- **热插拔：** Windows 使用 Configuration Manager 全接口通知，覆盖 HID、SERIAL、USB Print；Linux 监听 udev `hidraw` / `tty`；macOS 使用 IOKit `IOHIDDevice` / `IOSerialBSDClient` 通知。注册先于枚举。默认合并 40 ms 内的通知，并做有限重新枚举；另每 5 秒校准一次，覆盖未发出标准通知的虚拟端口。注册失败时创建管理器失败；运行中监听失败会发出错误，并继续校准枚举，可重建管理器恢复原生监听。
- **接收：** 串口返回任意长度的字节流片段，不包含业务组帧；HID 保留单个 Report 边界。`numbered_input_reports` 由调用方依据描述符选择，默认无编号；输入报告带 ID 时返回前剥离一次 ID，无编号报告首字节保留。输出与 Feature Report 的底层 ID 前缀由库添加；无编号报告传 0。
- **并发与顺序：** 会话可以克隆；共享一个发送队列和一个接收队列。发送按队列接纳顺序执行，并发调用不承诺调度顺序。多个接收者竞争同一输入，不做广播。`write` 允许部分写入，`write_all` 等待 OS 接受整个缓冲区，不代表远端确认或物理线路排空。
- **背压：** 输入和命令队列有界。默认各 64 条，串口每次读最多 4096 字节；HID 使用同一大小作为最大输入线缆报告缓冲区。发送数据默认上限 1 MiB，可配置。慢消费者会暂停后续读取，系统/设备缓冲区仍可能溢出，部分驱动不会报告这种丢失。大量并发调用方也应限制在途请求数，避免等待入队的数据占用过多内存。
- **超时与取消：** 默认操作超时 5 秒，包含入队和执行；`recv` 单独指定超时，`None` 无限等待。取消/超时会跳过尚未执行的命令并停止后续串口写入尝试，已经发送的数据无法撤回。HID 的部分写入/Feature 调用没有跨平台取消机制，可能在超时后才完成；不能承诺精确的停止时刻。
- **关闭与拔出：** `close` 幂等，通过独立控制通道终止工作，成功返回时句柄已释放；超时后关闭请求仍然有效。拔出后等待中的操作返回错误，接收队列中的旧数据不再保证可读。最后一个会话克隆被丢弃时请求关闭，但 `Drop` 不等待。管理器/订阅全部丢弃后停止监听，已有会话依然有效。
- **配置与错误：** 默认串口 115200、8N1、无流控；DTR/RTS 默认沿用后端行为，驱动可能在打开时产生跳变。配置按字段依次应用，失败可能已部分生效。不支持的配置可能由后端返回 `InvalidConfig` 或 `Backend`。保留底层错误信息；HID 后端只提供文本时不猜测其是否属于权限或占用错误。

## 代码结构

公开类型继续从 `usb_comm` 根目录导出，内部文件按职责组织：

| 路径 | 职责 |
|---|---|
| `src/device.rs` | 设备标识、元数据、事件和报告数据类型 |
| `src/filter.rs` | 单配置 AND、多配置 OR 及 HID Usage 筛选 |
| `src/options.rs` | 管理器、串口、HID、USB Print 配置与参数校验 |
| `src/discovery/` | 系统枚举与 USB Print 元数据补全 |
| `src/manager/` | 公开管理接口、后台快照维护、订阅事件处理 |
| `src/session/` | 公共队列与生命周期，以及各传输的收发实现 |
| `src/native/` | Windows / Linux / macOS 原生热插拔监听 |
| `src/runtime.rs`、`src/error.rs` | 共享运行时、同步桥接和错误分类 |

会话公共的状态、关闭和同步包装由同一份内部实现生成，各传输模块保留专用操作。同步发送在校验大小后只复制一次载荷，并把缓冲区所有权直接交给工作任务；USB Print 的事件等待使用固定数组，避免循环中的临时堆分配。关键接口、背压/取消行为及平台资源释放约束均有中文注释。

## 平台依赖

| 平台 | 构建 / 运行依赖 | 权限与限制 |
|---|---|---|
| Windows | Rust MSVC 工具链；HID、USB Print 使用原生 Rust 后端 | HID 使用系统 HID 驱动；串口需要系统或厂商串口驱动；USB Print 使用 `usbprint.sys`。部分系统 HID 集合受保护 |
| Linux | `pkg-config`、`libudev-dev`；运行时 `libudev` | HID 使用 hidraw 后端；需要设备节点权限或 udev 规则。串口用户通常需要加入 `dialout` / `uucp` |
| macOS | Xcode Command Line Tools；HID C 后端、系统 IOKit / CoreFoundation | HID 可能默认独占；沙盒、系统输入设备的权限由系统决定 |

Ubuntu / Debian 构建依赖：

```sh
sudo apt-get install pkg-config libudev-dev
```

Linux 可针对实际 VID/PID 部署权限规则，例如由桌面会话授予访问：

```text
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="1234", ATTRS{idProduct}=="5678", TAG+="uaccess"
```

后台服务应采用受控用户组和 `0660` 权限；库不修改驱动、用户组或系统权限。

## 示例与检查

```sh
cargo run --example discover
cargo run --example watch
cargo run --example serial_async -- COM3 hello
cargo run --example serial_sync -- COM3 hello
cargo run --example hid -- 1234 5678 --numbered
cargo run --example hid -- 1234 5678 --numbered 01 10 20
cargo run --example hid -- 1234 5678 --usage-page ff00 --usage 0001 --numbered
cargo run --example hid -- 1234 5678 --usage-page ff00 --usage 0001 --numbered 01 10 20
cargo run --example usb_print
cargo run --example usb_print -- read 0
cargo run --example usb_print -- send 0 01 02 03
cargo run --release --example serial_loopback -- 30 921600 COM3 COM4
```

`hid` 示例的 `--usage-page` / `--usage` 均为可选十六进制值，用于区分同 VID/PID 的 HID 接口；这些选项和 `--numbered` 放在 Report ID / payload 之前。不传 Report ID / payload 时仅读取；传入时发送 Output Report。`serial_loopback` 必须连接 TX/RX 回环或使用回显设备，会发送测试数据并验证回传，支持多个端口并行，输出吞吐及往返延迟 P95/P99。不要在业务设备上直接运行回环测试。

`usb_print` 默认仅列出 PTB 接口；`read` 打开指定序号读取；`send` 发送调用方给出的十六进制字节再等待输入。示例中的 `01 02 03` 仅说明字节输入方式，不是有效 PTB 业务帧，实际通信需使用设备协议编码的数据。

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo test --locked --doc
cargo doc --locked --no-deps
```

测试覆盖内存全双工、部分收发、背压、关闭/拔出中断、超时与取消、报告 ID、设备代次和事件溢出；Windows 另通过本地命名管道验证 USB Print 的实际重叠 I/O 和零字节调用，Unix 包含原生伪终端测试。三平台 CI 不需要真实设备。实机验收和性能测量见 [验证记录](docs/validation.md)。

当前不包含业务协议、自动重连/重发、驱动安装或通过 WinUSB/libusb 访问的自定义 USB Bulk 传输；不保证任意硬件的吞吐或插拔延迟。HID、USB Print 每个会话一个线程，数百个设备的资源占用需要实测。

## 许可证

MIT OR Apache-2.0。依赖各自许可证继续适用，其中 `serialport` 为 MPL-2.0。
