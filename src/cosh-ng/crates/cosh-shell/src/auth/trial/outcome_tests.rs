use super::*;

#[test]
fn rereview_fix_preference_settle_joins_original_worker_before_consuming_receipt() {
    let expected = [
        Ok(()),
        Err(AuthConfigureFailure {
            message: "session save failed".into(),
            code: Some("io_error".into()),
            provider_updated: true,
            unconfirmed: false,
        }),
        Err(AuthConfigureFailure {
            message: "EcsIdentityUnavailable".into(),
            code: Some("EcsIdentityUnavailable".into()),
            provider_updated: false,
            unconfirmed: false,
        }),
    ];
    for receipt in expected {
        let mut session = TrialSession::default();
        let (release, wait_release) = mpsc::channel();
        let (send_receipt, receive_receipt) = mpsc::channel();
        let (started, wait_started) = mpsc::channel();
        let (settled, wait_settled) = mpsc::channel();
        let receipt_for_worker = receipt.clone();
        session.preference_job = Some(receive_receipt);
        session.preference_worker = Some(std::thread::spawn(move || {
            wait_release.recv().unwrap();
            send_receipt.send(receipt_for_worker).unwrap();
        }));
        let settling = std::thread::spawn(move || {
            started.send(()).unwrap();
            let result = session.settle_preference_worker();
            settled.send(result).unwrap();
            assert!(session.preference_worker.is_none());
            assert!(session.preference_job.is_none());
        });
        wait_started.recv().unwrap();
        let early = wait_settled.recv_timeout(Duration::from_millis(20));
        release.send(()).unwrap();
        let result = early
            .clone()
            .or_else(|_| wait_settled.recv_timeout(Duration::from_secs(2)));
        settling.join().unwrap();
        assert!(matches!(early, Err(mpsc::RecvTimeoutError::Timeout)));
        assert_eq!(result.unwrap(), Some(receipt));
    }
}

#[test]
fn rereview_fix_preference_settle_missing_worker_receipt_is_unknown() {
    let mut session = TrialSession::default();
    let (send_receipt, receive_receipt) = mpsc::channel();
    session.preference_job = Some(receive_receipt);
    session.preference_worker = Some(std::thread::spawn(move || drop(send_receipt)));
    let error = session.settle_preference_worker().unwrap().unwrap_err();
    assert!(error.unconfirmed);
    assert!(!error.provider_updated);
    assert!(session.preference_job.is_none());
    assert!(session.preference_worker.is_none());
}
