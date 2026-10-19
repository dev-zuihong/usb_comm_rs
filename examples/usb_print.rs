use std::time::Duration;
use usb_comm::{DeviceFilter, DeviceManager, ManagerOptions, Transport, UsbPrintOptions};

/// PTB USB Print 示例：默认只列出设备，显式指定命令时才执行读写。
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("list");
    if !matches!(command, "list" | "read" | "send") {
        return Err("usage: usb_print [list | read <index> | send <index> <hex-byte> ...]".into());
    }
    let manager = DeviceManager::new(ManagerOptions::default())?;
    let devices = manager
        .discover(DeviceFilter {
            transport: Some(Transport::UsbPrint),
            vendor_id: Some(0x36b0),
            product_id: Some(0x5320),
            ..Default::default()
        })
        .await?;
    if command == "list" {
        for (index, device) in devices.iter().enumerate() {
            println!("[{index}] {device:#?}");
        }
        println!("{} PTB USB Print endpoint(s)", devices.len());
        return Ok(());
    }
    let index: usize = args
        .get(1)
        .ok_or("specify a device index from list")?
        .parse()?;
    let device = devices.get(index).ok_or("device index not found")?;
    let payload = if command == "send" {
        if args.len() < 3 {
            return Err("specify at least one hex payload byte".into());
        }
        args[2..]
            .iter()
            .map(|v| u8::from_str_radix(v.trim_start_matches("0x"), 16))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    let session = manager
        .open_usb_print(device, UsbPrintOptions::default())
        .await?;
    let result = async {
        if command == "send" {
            session.write_all(&payload).await?;
        }
        session.recv(Some(Duration::from_secs(5))).await
    }
    .await;
    session.close().await?;
    println!("{:02x?}", result?);
    Ok(())
}
