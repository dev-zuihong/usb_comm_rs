use super::*;

#[test]
fn input_report_id_is_removed_only_for_numbered_reports() {
    assert_eq!(
        decode_input(vec![7, 0, 1], true),
        HidReport {
            report_id: 7,
            payload: vec![0, 1]
        }
    );
    assert_eq!(
        decode_input(vec![7, 0, 1], false),
        HidReport {
            report_id: 0,
            payload: vec![7, 0, 1]
        }
    );
}

// 用固定报告校验同步/异步接口的线缆格式，不依赖真实 HID 外设。
fn report_session() -> HidSession {
    let (core, mut worker) = Core::new(IoOptions::default());
    let session = HidSession { core };
    runtime::handle().unwrap().spawn(async move {
        let command = worker.commands.recv().await.unwrap();
        let Operation::Write { data, .. } = command.operation else {
            panic!("expected report");
        };
        assert_eq!(data, [5, 10, 20]);
        command.reply.send(Ok(Output::Count(data.len()))).ok();
        let command = worker.commands.recv().await.unwrap();
        let Operation::FeatureWrite(data) = command.operation else {
            panic!("expected feature report");
        };
        assert_eq!(data, [0, 30]);
        command.reply.send(Ok(Output::Unit)).ok();
        let command = worker.commands.recv().await.unwrap();
        let Operation::FeatureRead { id, size } = command.operation else {
            panic!("expected feature read");
        };
        assert_eq!((id, size), (9, 4));
        command.reply.send(Ok(Output::Bytes(vec![9, 40, 50]))).ok();
        worker.lifecycle.finish(Error::Closed);
    });
    session
}

#[tokio::test]
async fn output_and_feature_reports_include_exactly_one_id() {
    let session = report_session();
    assert_eq!(session.send_report(5, &[10, 20]).await.unwrap(), 2);
    session.send_feature_report(0, &[30]).await.unwrap();
    assert_eq!(
        session.get_feature_report(9, 3).await.unwrap(),
        HidReport {
            report_id: 9,
            payload: vec![40, 50]
        }
    );
    session.close().await.unwrap();
}

#[test]
fn blocking_reports_keep_payload_counts_and_report_ids() {
    let session = report_session().into_blocking();
    assert_eq!(session.send_report(5, &[10, 20]).unwrap(), 2);
    session.send_feature_report(0, &[30]).unwrap();
    assert_eq!(
        session.get_feature_report(9, 3).unwrap(),
        HidReport {
            report_id: 9,
            payload: vec![40, 50],
        }
    );
    session.close().unwrap();
}
