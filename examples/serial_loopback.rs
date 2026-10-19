//! 每个指定串口都需要物理 TX/RX 回环或支持回显的对端。
use std::time::{Duration, Instant};
use usb_comm::{DeviceManager, IoOptions, ManagerOptions, SerialConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 4 {
        return Err("usage: serial_loopback <seconds> <baud> <port> [<port> ...]; requires physical loopback/echo".into());
    }
    let duration = Duration::from_secs(args[1].parse()?);
    if duration.is_zero() {
        return Err("seconds must be positive".into());
    }
    let baud_rate = args[2].parse()?;
    let manager = DeviceManager::new(ManagerOptions::default())?;
    let mut sessions = Vec::new();
    for path in &args[3..] {
        let session = manager
            .open_serial_path(
                path,
                SerialConfig {
                    baud_rate,
                    ..Default::default()
                },
                IoOptions::default(),
            )
            .await?;
        sessions.push((path.clone(), session));
    }
    println!(
        "pid={}, ports={}; monitor this process for CPU, memory and thread count",
        std::process::id(),
        sessions.len()
    );
    let mut jobs = Vec::new();
    for (path, session) in sessions {
        jobs.push(tokio::spawn(async move {
            let result = async {
                let payload: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
                let started = Instant::now();
                let mut latencies = Vec::new();
                let mut transferred = 0;
                while started.elapsed() < duration {
                    let roundtrip = Instant::now();
                    session.write_all(&payload).await?;
                    let mut received = Vec::new();
                    while received.len() < payload.len() {
                        received.extend(session.recv(Some(Duration::from_secs(5))).await?);
                    }
                    if received != payload {
                        return Err(usb_comm::Error::Backend {
                            backend: "loopback",
                            message: "payload mismatch".into(),
                        });
                    }
                    latencies.push(roundtrip.elapsed().as_secs_f64() * 1000.0);
                    transferred += payload.len();
                }
                latencies.sort_by(f64::total_cmp);
                let percentile =
                    |p: f64| latencies[((latencies.len() - 1) as f64 * p).ceil() as usize];
                println!(
                    "{path}: {:.0} bytes/s, roundtrip p95={:.2} ms p99={:.2} ms, samples={}",
                    transferred as f64 / started.elapsed().as_secs_f64(),
                    percentile(0.95),
                    percentile(0.99),
                    latencies.len()
                );
                Ok(())
            }
            .await;
            let close = session.close().await;
            result.and(close)
        }));
    }
    let mut failure = None;
    for job in jobs {
        if let Err(error) = job.await? {
            eprintln!("{error}");
            failure = Some(error);
        }
    }
    if let Some(error) = failure {
        return Err(error.into());
    }
    Ok(())
}
