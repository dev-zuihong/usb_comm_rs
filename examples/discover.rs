use usb_comm::{DeviceFilter, DeviceManager, ManagerOptions};

fn main() -> usb_comm::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let manager = DeviceManager::new(ManagerOptions::default())?;
    let devices = manager.discover_blocking(DeviceFilter::default())?;
    println!("{} endpoint(s)", devices.len());
    for device in devices {
        println!("{device:#?}");
    }
    Ok(())
}
