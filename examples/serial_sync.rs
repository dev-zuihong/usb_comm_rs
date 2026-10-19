use std::time::Duration;
use usb_comm::{DeviceManager, IoOptions, ManagerOptions, SerialConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("usage: serial_sync <port> <text-to-send>".into());
    }
    let manager = DeviceManager::new(ManagerOptions::default())?;
    let serial = manager.open_serial_path_blocking(
        &args[1],
        SerialConfig::default(),
        IoOptions::default(),
    )?;
    serial.write_all(args[2].as_bytes())?;
    let result = serial.recv(Some(Duration::from_secs(2)));
    serial.close()?;
    println!("{:02x?}", result?);
    Ok(())
}
