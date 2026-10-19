use super::*;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

impl Port for DuplexStream {}

#[tokio::test]
async fn sixty_four_sessions_progress_on_the_shared_runtime() {
    let mut jobs = Vec::new();
    for index in 0..64_u8 {
        let (session, mut peer) = pair(
            IoOptions {
                receive_capacity: 2,
                read_buffer_size: 37,
                ..Default::default()
            },
            8,
        )
        .await;
        jobs.push(tokio::spawn(async move {
            let echo = tokio::spawn(async move {
                let mut buffer = [0; 2048];
                peer.read_exact(&mut buffer).await.unwrap();
                peer.write_all(&buffer).await.unwrap();
                let _ = peer.read(&mut [0; 1]).await;
            });
            session.write_all(&vec![index; 2048]).await.unwrap();
            assert_eq!(collect(&session, 2048).await, vec![index; 2048]);
            session.close().await.unwrap();
            echo.await.unwrap();
        }));
    }
    tokio::time::timeout(Duration::from_secs(15), async {
        for job in jobs {
            job.await.unwrap();
        }
    })
    .await
    .unwrap();
}

async fn pair(options: IoOptions, capacity: usize) -> (SerialSession, DuplexStream) {
    runtime::run(async move {
        let (port, peer) = tokio::io::duplex(capacity);
        let buffer_size = options.read_buffer_size;
        let (core, worker) = Core::new(options);
        tokio::spawn(serve(port, worker, buffer_size));
        Ok((SerialSession { core }, peer))
    })
    .await
    .unwrap()
}

async fn collect(session: &SerialSession, count: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    while bytes.len() < count {
        bytes.extend(session.recv(Some(Duration::from_secs(2))).await.unwrap());
    }
    bytes
}

#[tokio::test]
async fn full_duplex_partial_io_with_receive_backpressure() {
    let options = IoOptions {
        receive_capacity: 1,
        read_buffer_size: 13,
        ..Default::default()
    };
    let (session, peer) = pair(options, 8).await;
    let outgoing: Vec<u8> = (0..16_384).map(|i| (i % 251) as u8).collect();
    let incoming: Vec<u8> = (0..16_384).map(|i| (i % 239) as u8).collect();
    let expected_incoming = incoming.clone();
    let peer_job = tokio::spawn(async move {
        let (mut reader, mut writer) = tokio::io::split(peer);
        let read = async {
            let mut bytes = vec![0; 16_384];
            reader.read_exact(&mut bytes).await.unwrap();
            bytes
        };
        let write = async {
            writer.write_all(&incoming).await.unwrap();
        };
        let (bytes, ()) = tokio::join!(read, write);
        // 保留对端直到会话读完最后一个片段，避免 EOF 提前覆盖输入。
        (bytes, reader, writer)
    });
    let (sent, received) = tokio::join!(session.write_all(&outgoing), collect(&session, 16_384));
    sent.unwrap();
    assert_eq!(received, expected_incoming);
    let (bytes, _reader, _writer) = peer_job.await.unwrap();
    assert_eq!(bytes, outgoing);
    session.close().await.unwrap();
}

#[tokio::test]
async fn receive_timeout_does_not_consume_future_input() {
    let (session, mut peer) = pair(IoOptions::default(), 32).await;
    assert!(matches!(
        session.recv(Some(Duration::from_millis(10))).await,
        Err(Error::Timeout)
    ));
    peer.write_all(b"after timeout").await.unwrap();
    assert_eq!(collect(&session, 13).await, b"after timeout");
    session.close().await.unwrap();
}

#[tokio::test]
async fn close_interrupts_pending_write_and_receive_and_is_idempotent() {
    let (session, mut peer) = pair(IoOptions::default(), 8).await;
    let writing = tokio::spawn({
        let session = session.clone();
        async move { session.write_all(&vec![7; 65_536]).await }
    });
    let mut first = [0; 1];
    peer.read_exact(&mut first).await.unwrap();
    let receiving = tokio::spawn({
        let session = session.clone();
        async move { session.recv(None).await }
    });
    session.close().await.unwrap();
    assert!(matches!(writing.await.unwrap(), Err(Error::Closed)));
    assert!(matches!(receiving.await.unwrap(), Err(Error::Closed)));
    assert_eq!(session.state(), SessionState::Closed);
    session.close().await.unwrap();
    assert!(matches!(session.write(b"x").await, Err(Error::Closed)));
}

#[tokio::test]
async fn canceled_write_without_deadline_does_not_block_next_command() {
    let options = IoOptions {
        operation_timeout: None,
        ..Default::default()
    };
    let (session, mut peer) = pair(options, 8).await;
    let writing = tokio::spawn({
        let session = session.clone();
        async move { session.write_all(&vec![7; 65_536]).await }
    });
    peer.read_exact(&mut [0; 1]).await.unwrap();
    writing.abort();
    let _ = writing.await;
    tokio::time::timeout(Duration::from_secs(1), session.write_all(&[]))
        .await
        .unwrap()
        .unwrap();
    session.close().await.unwrap();
}

#[tokio::test]
async fn timed_out_write_does_not_poison_session() {
    let options = IoOptions {
        operation_timeout: Some(Duration::from_millis(30)),
        ..Default::default()
    };
    let (session, mut peer) = pair(options, 8).await;
    assert!(matches!(
        session.write_all(&vec![1; 65_536]).await,
        Err(Error::Timeout)
    ));
    // 超时无法撤回已经写出的字节，但后续队列操作应能继续执行。
    let mut first = [0; 8];
    peer.read_exact(&mut first).await.unwrap();
    session.write_all(b"ok").await.unwrap();
    let mut next = [0; 2];
    peer.read_exact(&mut next).await.unwrap();
    assert_eq!(&next, b"ok");
    session.close().await.unwrap();
}

#[tokio::test]
async fn dropping_peer_records_terminal_disconnect() {
    let (session, peer) = pair(IoOptions::default(), 8).await;
    drop(peer);
    assert!(matches!(
        session.recv(Some(Duration::from_secs(1))).await,
        Err(Error::Disconnected(_))
    ));
    assert_eq!(session.state(), SessionState::Disconnected);
    assert!(matches!(
        session.terminal_error(),
        Some(Error::Disconnected(_))
    ));
    session.close().await.unwrap();
}

#[tokio::test]
async fn native_removal_interrupts_receive_even_when_queue_is_full() {
    let options = IoOptions {
        receive_capacity: 1,
        read_buffer_size: 1,
        ..Default::default()
    };
    let (session, mut peer) = pair(options, 8).await;
    peer.write_all(&[1; 8]).await.unwrap();
    session.core.disconnect("test removal");
    assert!(matches!(
        session.recv(None).await,
        Err(Error::Disconnected(_))
    ));
    session.close().await.unwrap();
}

#[tokio::test]
async fn last_clone_drop_releases_transport() {
    let (session, mut peer) = pair(IoOptions::default(), 8).await;
    let second = session.clone();
    drop(session);
    second.write_all(b"x").await.unwrap();
    peer.read_exact(&mut [0; 1]).await.unwrap();
    drop(second);
    let result = tokio::time::timeout(Duration::from_secs(1), peer.read(&mut [0; 1]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result, 0);
}

#[tokio::test]
async fn blocking_api_rejects_tokio_without_closing_session() {
    let (session, _peer) = pair(IoOptions::default(), 8).await;
    let blocking = session.clone().into_blocking();
    assert!(matches!(blocking.close(), Err(Error::InvalidContext)));
    assert_eq!(session.state(), SessionState::Open);
    session.close().await.unwrap();
}

#[test]
fn blocking_api_uses_same_transport_and_errors() {
    let (session, mut peer) =
        runtime::blocking(async { Ok(pair(IoOptions::default(), 32).await) }).unwrap();
    runtime::handle().unwrap().spawn(async move {
        let mut bytes = [0; 5];
        peer.read_exact(&mut bytes).await.unwrap();
        peer.write_all(&bytes).await.unwrap();
        // 等待关闭后再释放对端，避免消费者读取前先收到 EOF。
        let _ = peer.read(&mut [0; 1]).await;
    });
    let session = session.into_blocking();
    session.write_all(b"hello").unwrap();
    assert_eq!(
        session.recv(Some(Duration::from_secs(1))).unwrap(),
        b"hello"
    );
    session.close().unwrap();
    assert!(matches!(session.recv(None), Err(Error::Closed)));
}

#[tokio::test]
async fn write_limit_is_checked_before_queueing() {
    let options = IoOptions {
        max_write_size: 4,
        ..Default::default()
    };
    let (session, _peer) = pair(options, 8).await;
    assert!(matches!(
        session.write_all(b"12345").await,
        Err(Error::InvalidConfig(_))
    ));
    assert_eq!(session.write(b"").await.unwrap(), 0);
    session.close().await.unwrap();
}

#[tokio::test]
async fn configuration_failure_does_not_close_transport() {
    let (session, _peer) = pair(IoOptions::default(), 8).await;
    assert!(matches!(
        session.configure(SerialConfig::default()).await,
        Err(Error::Unsupported(_))
    ));
    session.write_all(b"still ok").await.unwrap();
    session.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn native_pseudo_terminal_uses_serial_backend() {
    runtime::run(async {
        let (port, mut peer) = SerialStream::pair()?;
        let (core, worker) = Core::new(IoOptions::default());
        tokio::spawn(serve(port, worker, 4096));
        let session = SerialSession { core };
        session.write_all(b"native serial").await?;
        let mut bytes = [0; 13];
        peer.read_exact(&mut bytes).await?;
        assert_eq!(&bytes, b"native serial");
        peer.write_all(b"reply").await?;
        assert_eq!(session.recv(Some(Duration::from_secs(1))).await?, b"reply");
        session.close().await?;
        Ok(())
    })
    .await
    .unwrap();
}
