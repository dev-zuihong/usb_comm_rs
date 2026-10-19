use std::time::Duration;
use usb_comm::{DeviceManager, IoOptions, ManagerOptions, SerialConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("usage: serial_async <port> <text-to-send>".into());
    }
    let manager = DeviceManager::new(ManagerOptions::default())?;
    let serial = manager
        .open_serial_path(&args[1], SerialConfig::default(), IoOptions::default())
        .await?;
    serial.write_all(args[2].as_bytes()).await?;
    let result = serial.recv(Some(Duration::from_secs(2))).await;
    serial.close().await?;
    println!("{:02x?}", result?);
    Ok(())
}
