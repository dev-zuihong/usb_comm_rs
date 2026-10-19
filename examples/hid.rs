use std::time::Duration;
use usb_comm::{DeviceFilter, DeviceManager, HidOptions, ManagerOptions, Transport};

fn hex(value: &str) -> Result<u16, std::num::ParseIntError> {
    u16::from_str_radix(value.trim_start_matches("0x"), 16)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 3 {
        return Err(
            "usage: hid <vid-hex> <pid-hex> [--usage-page <hex>] [--usage <hex>] [--numbered] [<report-id-hex> <payload-byte-hex> ...]"
                .into(),
        );
    }
    let mut filter = DeviceFilter {
        transport: Some(Transport::Hid),
        vendor_id: Some(hex(&args[1])?),
        product_id: Some(hex(&args[2])?),
        ..Default::default()
    };
    let mut numbered = false;
    let mut first = 3;
    while let Some(option) = args.get(first).filter(|v| v.starts_with("--")) {
        match option.as_str() {
            "--numbered" => numbered = true,
            "--usage-page" | "--usage" => {
                first += 1;
                let value = args
                    .get(first)
                    .ok_or_else(|| format!("{option} requires a hexadecimal value"))?;
                let value = hex(value)?;
                if option == "--usage-page" {
                    filter.usage_page = Some(value);
                } else {
                    filter.usage = Some(value);
                }
            }
            _ => return Err(format!("unknown option: {option}").into()),
        }
        first += 1;
    }
    // 在发现或打开设备之前检查全部参数。
    let output = args
        .get(first)
        .map(|id| -> Result<_, Box<dyn std::error::Error>> {
            let id = u8::try_from(hex(id)?)?;
            let payload = args[first + 1..]
                .iter()
                .map(|v| Ok(u8::try_from(hex(v)?)?))
                .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
            Ok((id, payload))
        })
        .transpose()?;
    let manager = DeviceManager::new(ManagerOptions::default())?;
    let devices = manager.discover(filter).await?;
    if devices.len() != 1 {
        return Err(format!(
            "expected one endpoint, found {}; use --usage-page/--usage or select a specific path/serial number in your application",
            devices.len()
        )
        .into());
    }
    let session = manager
        .open_hid(
            &devices[0],
            HidOptions {
                numbered_input_reports: numbered,
                ..Default::default()
            },
        )
        .await?;
    if let Some((id, payload)) = output {
        session.send_report(id, &payload).await?;
    }
    let result = session.recv(Some(Duration::from_secs(5))).await;
    session.close().await?;
    println!("{:02x?}", result?);
    Ok(())
}
