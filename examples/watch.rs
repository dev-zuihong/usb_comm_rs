use usb_comm::{DeviceEvent, DeviceFilter, DeviceManager, ManagerOptions};

#[tokio::main]
async fn main() -> usb_comm::Result<()> {
    let manager = DeviceManager::new(ManagerOptions::default())?;
    let mut watcher = manager.watch(DeviceFilter::default());
    loop {
        match watcher.recv().await? {
            DeviceEvent::ResyncRequired { missed } => {
                eprintln!("missed {missed} events; rebuilding subscription");
                manager.discover(DeviceFilter::default()).await?;
                watcher = manager.watch(DeviceFilter::default());
            }
            event => println!("{event:#?}"),
        }
    }
}
