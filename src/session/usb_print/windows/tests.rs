use super::*;
use std::{
    fs::File,
    io::{Read, Write},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use windows_sys::Win32::System::Pipes::*;

/// 通过本地 Windows 字节管道验证真实重叠 I/O 引擎。
/// 这些测试不打开物理打印设备，也不发送 PTB 业务命令。
fn pipe(write: bool, size: usize) -> (Transfer, File) {
    static NEXT_PIPE: AtomicUsize = AtomicUsize::new(0);
    let name = format!(
        r"\\.\pipe\usb-comm-test-{}-{}",
        std::process::id(),
        NEXT_PIPE.fetch_add(1, Ordering::Relaxed)
    );
    let path: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: 创建唯一命名的本地同步字节管道，服务端句柄由本测试持有。
    let server = unsafe {
        CreateNamedPipeW(
            path.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,
            64,
            64,
            0,
            ptr::null(),
        )
    };
    assert_ne!(
        server,
        INVALID_HANDLE_VALUE,
        "{}",
        io::Error::last_os_error()
    );
    let server = unsafe { OwnedHandle::from_raw_handle(server) };
    let transfer = Transfer::open(&path, write, size).unwrap();
    // 客户端已在此调用前连接，返回 ERROR_PIPE_CONNECTED 属于正常情况。
    let connected = unsafe { ConnectNamedPipe(server.as_raw_handle(), ptr::null_mut()) };
    assert!(
        connected != 0
            || io::Error::last_os_error().raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32)
    );
    (transfer, File::from(server))
}

fn session(options: UsbPrintOptions) -> (UsbPrintSession, File, File, Arc<AtomicUsize>) {
    // Transfer 内含 OVERLAPPED 裸指针，应在同一专用线程创建并使用，
    // 不通过声明 Send 将其跨线程转移。
    let (ready, result) = std::sync::mpsc::sync_channel(1);
    thread::spawn(move || {
        let (read, incoming) = pipe(false, options.io.read_buffer_size);
        let (write, outgoing) = pipe(true, 0);
        let zero_length_writes = write.zero_length_writes.clone();
        let wake = Arc::new(Event::new(false).unwrap());
        let callback_wake = wake.clone();
        let (mut core, worker) = Core::new(options.io.clone());
        Arc::get_mut(&mut core).unwrap().wake = Some(Arc::new(move || callback_wake.signal()));
        ready
            .send((
                UsbPrintSession { core },
                incoming,
                outgoing,
                zero_length_writes,
            ))
            .ok();
        serve(read, write, worker, &options, &wake);
    });
    result.recv().unwrap()
}

#[tokio::test]
async fn overlapped_full_duplex_zlp_and_close_cancel_pending_read() {
    let (session, mut incoming, mut outgoing, zlp) = session(UsbPrintOptions::default());
    let reader = tokio::task::spawn_blocking(move || {
        let mut bytes = vec![0; 64 + 65 + 128];
        outgoing.read_exact(&mut bytes).unwrap();
        // close 回收两个句柄期间保留输出端连接。
        (bytes, outgoing)
    });
    let writer = tokio::task::spawn_blocking(move || {
        incoming.write_all(b"PTB input").unwrap();
        incoming
    });
    session.write_all(&[1; 64]).await.unwrap();
    session.write_all(&[2; 65]).await.unwrap();
    session.write_all(&[3; 128]).await.unwrap();
    session.write_all(&[]).await.unwrap();
    let mut received = Vec::new();
    while received.len() < 9 {
        received.extend(session.recv(Some(Duration::from_secs(2))).await.unwrap());
    }
    assert_eq!(received, b"PTB input");
    let (bytes, _outgoing) = reader.await.unwrap();
    let _incoming = writer.await.unwrap();
    assert_eq!(&bytes[..64], &[1; 64]);
    assert_eq!(&bytes[64..129], &[2; 65]);
    assert_eq!(&bytes[129..], &[3; 128]);
    assert_eq!(zlp.load(Ordering::Relaxed), 2);
    session.close().await.unwrap();
    assert_eq!(session.state(), SessionState::Closed);
    session.close().await.unwrap();
}

#[tokio::test]
async fn packet_size_and_disabled_zlp_are_respected_by_the_worker() {
    for (size, enabled, expected) in [(512, true, 0), (64, false, 0), (64, true, 1)] {
        let options = UsbPrintOptions {
            bulk_max_packet_size: size,
            send_zero_length_packet: enabled,
            ..Default::default()
        };
        let (session, _incoming, mut outgoing, zlp) = session(options);
        let reader = tokio::task::spawn_blocking(move || {
            outgoing.read_exact(&mut [0; 64]).unwrap();
            outgoing
        });
        session.write_all(&[7; 64]).await.unwrap();
        let _outgoing = reader.await.unwrap();
        assert_eq!(zlp.load(Ordering::Relaxed), expected);
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn close_cancels_pending_native_write_and_read() {
    let options = UsbPrintOptions {
        io: IoOptions {
            operation_timeout: None,
            ..Default::default()
        },
        ..Default::default()
    };
    let (session, _incoming, _outgoing, _) = session(options);
    let write = tokio::spawn({
        let session = session.clone();
        async move { session.write_all(&vec![4; 1024 * 1024]).await }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    tokio::time::timeout(Duration::from_secs(3), session.close())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(write.await.unwrap(), Err(Error::Closed)));
}

#[tokio::test]
async fn write_deadline_cancels_native_io_without_poisoning_session() {
    let options = UsbPrintOptions {
        io: IoOptions {
            operation_timeout: Some(Duration::from_millis(40)),
            ..Default::default()
        },
        ..Default::default()
    };
    let (session, _incoming, _outgoing, _) = session(options);
    assert!(matches!(
        session.write_all(&vec![4; 1024 * 1024]).await,
        Err(Error::Timeout)
    ));
    session.write_all(&[]).await.unwrap();
    assert_eq!(session.state(), SessionState::Open);
    session.close().await.unwrap();
}

#[tokio::test]
async fn native_input_backpressure_keeps_writes_and_capacity_wake_working() {
    let options = UsbPrintOptions {
        io: IoOptions {
            receive_capacity: 1,
            read_buffer_size: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let (session, mut incoming, mut outgoing, _) = session(options);
    let input = tokio::task::spawn_blocking(move || {
        incoming.write_all(b"abc").unwrap();
        incoming
    });
    let output = tokio::task::spawn_blocking(move || {
        let mut data = [0; 5];
        outgoing.read_exact(&mut data).unwrap();
        (data, outgoing)
    });
    let _incoming = input.await.unwrap();
    session.write_all(b"awake").await.unwrap();
    let (bytes, _outgoing) = output.await.unwrap();
    assert_eq!(&bytes, b"awake");
    let mut received = Vec::new();
    while received.len() < 3 {
        received.extend(session.recv(Some(Duration::from_secs(2))).await.unwrap());
    }
    assert_eq!(received, b"abc");
    session.close().await.unwrap();
}
