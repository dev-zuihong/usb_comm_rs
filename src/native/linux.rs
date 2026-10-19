//! 通过 udev 监听 hidraw/tty，使用可中断的 poll 等待通知。

use super::*;
use std::os::fd::AsRawFd;

pub(super) fn run(
    stop: &AtomicBool,
    signal: &Signal,
    ready: mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    let socket = udev::MonitorBuilder::new()
        .and_then(|v| v.match_subsystem("hidraw"))
        .and_then(|v| v.match_subsystem("tty"))
        .and_then(|v| v.listen());
    let socket = match socket {
        Ok(socket) => socket,
        Err(error) => {
            let error = Error::from(error);
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
    };
    let _ = ready.send(Ok(()));
    while !stop.load(Ordering::Acquire) {
        let mut fd = libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: fd 指向一个有效 pollfd，描述符在此期间由 socket 持有。
        let count = unsafe { libc::poll(&mut fd, 1, 100) };
        if count < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(Error::Backend {
                backend: "udev",
                message: "monitor socket closed".into(),
            });
        }
        if socket.iter().next().is_some() {
            for _ in socket.iter() {}
            signal.notify();
        }
    }
    Ok(())
}
