use super::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use futures::{stream, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncWriteExt, BufReader, DuplexStream};
use tokio::sync::mpsc;

use crate::protocol::ecs_trial::TrialIntent;
use crate::provider::{DispatchState, GenerateEvent, ProviderFailureKind};

enum Reply {
    Events(Vec<GenerateEvent>),
    Pending,
    Tail(Duration, Duration),
    AcceptedTail(Duration, Duration, Duration, Option<ProviderFailure>),
}

struct Fixture {
    replies: Mutex<VecDeque<Reply>>,
    calls: AtomicUsize,
    dropped: Arc<AtomicUsize>,
}

struct Dropped(Arc<AtomicUsize>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl ContentGenerator for Fixture {
    async fn generate(
        &self,
        _: &[Message],
        _: &[ToolDeclaration],
        _: &GenerateConfig,
    ) -> Result<GenerateStream, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let guard = Dropped(self.dropped.clone());
        let reply =
            self.replies.lock().unwrap().pop_front().unwrap_or_else(|| {
                Reply::Events(vec![GenerateEvent::ProviderFailure(activation())])
            });
        match reply {
            Reply::Pending => {
                let _guard = guard;
                std::future::pending().await
            }
            Reply::Events(events) => Ok(Box::pin(stream::unfold(
                (events.into_iter(), guard),
                |(mut events, guard)| async move { events.next().map(|event| (event, (events, guard))) },
            ))),
            Reply::AcceptedTail(header_delay, token_delay, tail_delay, failure) => {
                tokio::time::sleep(header_delay).await;
                let accepted = stream::once(async { GenerateEvent::RequestAccepted });
                let body = stream::once(async move {
                    tokio::time::sleep(token_delay).await;
                    failure
                        .map(GenerateEvent::ProviderFailure)
                        .unwrap_or_else(|| GenerateEvent::TextDelta("after-headers".into()))
                });
                let tail = stream::once(async move {
                    let _guard = guard;
                    tokio::time::sleep(tail_delay).await;
                    GenerateEvent::MessageEnd
                });
                Ok(Box::pin(accepted.chain(body).chain(tail)))
            }
            Reply::Tail(head_delay, tail_delay) => {
                tokio::time::sleep(head_delay).await;
                Ok(Box::pin(
                    stream::once(async { GenerateEvent::TextDelta("first".into()) }).chain(
                        stream::once(async move {
                            let _guard = guard;
                            tokio::time::sleep(tail_delay).await;
                            GenerateEvent::MessageEnd
                        }),
                    ),
                ))
            }
        }
    }
    fn cancel(&self) {}
}

fn activation() -> ProviderFailure {
    ProviderFailure {
        code: "Sysom.ServiceNotActivated".into(),
        kind: ProviderFailureKind::ActivationRequired,
        dispatch: DispatchState::NotDispatched,
        request_id: None,
    }
}
fn events() -> Reply {
    Reply::Events(vec![
        GenerateEvent::TextDelta("answer".into()),
        GenerateEvent::MessageEnd,
    ])
}
fn context() -> EcsTrialContext {
    EcsTrialContext {
        schema_version: 1,
        shell_session_id: "session".into(),
        intent: TrialIntent::UserInitiated,
        auto_use_allowed: true,
    }
}
fn fixture(
    replies: Vec<Reply>,
    preference: Arc<dyn Fn() -> bool + Send + Sync>,
) -> (Box<dyn ContentGenerator>, Arc<TrialAccess>, Arc<Fixture>) {
    let inner = Arc::new(Fixture {
        replies: Mutex::new(replies.into()),
        calls: AtomicUsize::new(0),
        dropped: Arc::new(AtomicUsize::new(0)),
    });
    let (provider, access) = wrap_with_check(inner.clone(), context(), preference);
    (provider, access, inner)
}

struct Capture {
    bytes: Vec<u8>,
    line: Vec<u8>,
    tx: mpsc::UnboundedSender<Value>,
}
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        for byte in bytes {
            if *byte == b'\n' {
                if let Ok(value) = serde_json::from_slice(&self.line) {
                    let _ = self.tx.send(value);
                }
                self.line.clear();
            } else {
                self.line.push(*byte);
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn io() -> (
    DuplexStream,
    Lines<BufReader<DuplexStream>>,
    Capture,
    mpsc::UnboundedReceiver<Value>,
) {
    let (client, server) = tokio::io::duplex(16_384);
    let (tx, rx) = mpsc::unbounded_channel();
    (
        client,
        BufReader::new(server).lines(),
        Capture {
            bytes: Vec::new(),
            line: Vec::new(),
            tx,
        },
        rx,
    )
}
async fn collect(provider: &dyn ContentGenerator) -> Result<Vec<GenerateEvent>, String> {
    let events: Vec<GenerateEvent> = provider
        .generate(&[], &[], &GenerateConfig::default())
        .await?
        .collect()
        .await;
    if let Some(GenerateEvent::ProviderFailure(failure)) = events.first() {
        return Err(failure.code.clone());
    }
    Ok(events)
}
fn control(progress: &Value, revision: u64, action: &str) -> String {
    format!(
        "{}\n",
        json!({"type":"ecs_trial_control","run_id":progress["run_id"],"flow_id":progress["flow_id"],"revision":revision,"action":action})
    )
}
async fn next_phase(rx: &mut mpsc::UnboundedReceiver<Value>, phase: &str) -> Value {
    loop {
        let value = rx.recv().await.unwrap();
        if value["phase"] == phase {
            return value;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn activation_retries_the_same_step_and_keeps_the_first_real_event() {
    let (provider, access, inner) = fixture(
        vec![
            Reply::Events(vec![GenerateEvent::ProviderFailure(activation())]),
            events(),
        ],
        Arc::new(|| true),
    );
    let (_client, mut reader, mut writer, _) = io();
    let started = tokio::time::Instant::now();
    let result = drive(Some(&access), collect(&*provider), &mut reader, &mut writer)
        .await
        .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    assert_eq!(started.elapsed(), Duration::from_secs(5));
    assert!(matches!(&result[0], GenerateEvent::TextDelta(text) if text=="answer"));
    let states: Vec<Value> = String::from_utf8(writer.bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        states
            .iter()
            .filter(|state| state["phase"] == "ready")
            .count(),
        1
    );
    assert!(access.failure().is_none());
}

#[tokio::test(start_paused = true)]
async fn only_exact_predispatch_commercial_failures_may_poll() {
    for failure in [
        ProviderFailure {
            dispatch: DispatchState::Unknown,
            ..activation()
        },
        ProviderFailure {
            dispatch: DispatchState::Accepted,
            ..activation()
        },
        ProviderFailure {
            code: "Sysom.NotActuallyActivated".into(),
            ..activation()
        },
        ProviderFailure {
            kind: ProviderFailureKind::PermissionDenied,
            ..activation()
        },
        ProviderFailure {
            code: "Sysom.CreditsExhausted".into(),
            kind: ProviderFailureKind::QuotaExhausted,
            ..activation()
        },
    ] {
        let (provider, access, inner) = fixture(
            vec![
                Reply::Events(vec![GenerateEvent::ProviderFailure(failure.clone())]),
                events(),
            ],
            Arc::new(|| true),
        );
        let (_client, mut reader, mut writer, _) = io();
        assert!(
            drive(Some(&access), collect(&*provider), &mut reader, &mut writer)
                .await
                .is_err()
        );
        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
        assert_eq!(access.failure(), Some(failure));
    }
}

#[tokio::test(start_paused = true)]
async fn preference_is_checked_before_initial_and_retry_http() {
    let allowed = Arc::new(AtomicBool::new(false));
    let flag = allowed.clone();
    let (provider, access, inner) = fixture(
        vec![events()],
        Arc::new(move || flag.load(Ordering::SeqCst)),
    );
    let (_client, mut reader, mut writer, _) = io();
    assert!(
        drive(Some(&access), collect(&*provider), &mut reader, &mut writer)
            .await
            .is_err()
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    let checks = Arc::new(AtomicUsize::new(0));
    let count = checks.clone();
    let (provider, access, inner) = fixture(
        vec![
            Reply::Events(vec![GenerateEvent::ProviderFailure(activation())]),
            events(),
        ],
        Arc::new(move || count.fetch_add(1, Ordering::SeqCst) == 0),
    );
    let (_client, mut reader, mut writer, _) = io();
    assert!(
        drive(Some(&access), collect(&*provider), &mut reader, &mut writer)
            .await
            .is_err()
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!(checks.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn admission_has_a_ten_minute_and_seventy_attempt_budget() {
    let (provider, access, inner) = fixture(Vec::new(), Arc::new(|| true));
    let (_client, mut reader, mut writer, _) = io();
    let start = tokio::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(601),
        drive(Some(&access), collect(&*provider), &mut reader, &mut writer),
    )
    .await
    .expect("admission failed to stop");
    assert!(result.is_err());
    assert!(inner.calls.load(Ordering::SeqCst) <= 70);
    assert!(start.elapsed() <= Duration::from_secs(600));
}

#[tokio::test(start_paused = true)]
async fn acceptance_at_599_seconds_removes_the_admission_deadline() {
    let (provider, access, inner) = fixture(
        vec![Reply::Tail(
            Duration::from_secs(599),
            Duration::from_secs(2),
        )],
        Arc::new(|| true),
    );
    let (_client, mut reader, mut writer, _) = io();
    let start = tokio::time::Instant::now();
    let result = drive(Some(&access), collect(&*provider), &mut reader, &mut writer)
        .await
        .unwrap();
    assert_eq!(start.elapsed(), Duration::from_secs(601));
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(result.last(), Some(GenerateEvent::MessageEnd)));
    assert!(!String::from_utf8(writer.bytes)
        .unwrap()
        .contains("\"phase\":\"ready\""));
}

#[tokio::test(start_paused = true)]
async fn unrelated_input_does_not_recreate_the_pinned_future() {
    let (_, access, _) = fixture(vec![], Arc::new(|| true));
    let (mut client, mut reader, mut writer, _) = io();
    client
        .write_all(b"{\"type\":\"config_override\"}\n{\"type\":\"user\",\"message\":\"later\"}\n")
        .await
        .unwrap();
    let calls = AtomicUsize::new(0);
    let result = drive(
        Some(&access),
        async {
            calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(1)).await;
            42
        },
        &mut reader,
        &mut writer,
    )
    .await;
    assert_eq!(result, 42);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(access.drain_input().len(), 2);
    assert!(access.drain_input().is_empty());
}

#[tokio::test(start_paused = true)]
async fn interrupt_drops_an_inflight_generate_before_ack_and_blocks_next_step() {
    let (provider, access, inner) = fixture(vec![Reply::Pending, events()], Arc::new(|| true));
    let (mut client, mut reader, mut writer, mut rx) = io();
    let controller = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        client
            .write_all(b"{\"type\":\"control_request\",\"request_id\":\"interrupt-1\",\"request\":{\"subtype\":\"interrupt\"}}\n")
            .await
            .unwrap();
        let terminal = next_phase(&mut rx, "terminated").await;
        assert_eq!(terminal["local_terminated"], true);
        assert_eq!(inner.dropped.load(Ordering::SeqCst), 1);
    };
    let operation = async {
        assert!(
            drive(Some(&access), collect(&*provider), &mut reader, &mut writer)
                .await
                .is_err()
        );
        assert!(collect(&*provider).await.is_err());
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(operation, controller);
    })
    .await
    .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn waiting_pause_is_resumable_and_old_or_skipped_revisions_do_not_activate() {
    let (provider, access, inner) = fixture(
        vec![
            Reply::Events(vec![GenerateEvent::ProviderFailure(activation())]),
            events(),
        ],
        Arc::new(|| true),
    );
    let (mut client, mut reader, mut writer, mut rx) = io();
    let controller = async {
        let waiting = next_phase(&mut rx, "waiting_activation").await;
        assert_eq!(waiting["revision"], 1);
        client
            .write_all(control(&waiting, 2, "pause").as_bytes())
            .await
            .unwrap();
        let paused = next_phase(&mut rx, "paused").await;
        assert_eq!(paused["dispatch_state"], "not_dispatched");
        assert_eq!(paused["resume_allowed"], true);
        for revision in [1, 2, 4] {
            client
                .write_all(control(&waiting, revision, "resume").as_bytes())
                .await
                .unwrap();
        }
        tokio::time::sleep(Duration::from_secs(20)).await;
        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
        client
            .write_all(control(&waiting, 3, "resume").as_bytes())
            .await
            .unwrap();
    };
    let operation =
        async { drive(Some(&access), collect(&*provider), &mut reader, &mut writer).await };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(100), async {
        tokio::join!(operation, controller)
    })
    .await
    .unwrap();
    assert!(result.is_ok());
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn a_partial_stream_is_never_replayed_after_a_later_failure() {
    let (provider, access, inner) = fixture(
        vec![
            Reply::Events(vec![
                GenerateEvent::TextDelta("partial".into()),
                GenerateEvent::ProviderFailure(activation()),
            ]),
            events(),
        ],
        Arc::new(|| true),
    );
    let (_client, mut reader, mut writer, _) = io();
    let result = drive(Some(&access), collect(&*provider), &mut reader, &mut writer)
        .await
        .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(&result[0],GenerateEvent::TextDelta(text) if text=="partial"));
    assert_eq!(access.failure().unwrap().dispatch, DispatchState::Accepted);
}

#[tokio::test(start_paused = true)]
async fn interrupt_wakes_and_drops_an_accepted_silent_stream() {
    let (provider, access, inner) = fixture(
        vec![Reply::Tail(Duration::ZERO, Duration::from_secs(1000))],
        Arc::new(|| true),
    );
    let (mut client, mut reader, mut writer, mut rx) = io();
    let controller = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        client
            .write_all(b"{\"type\":\"control_request\",\"request_id\":\"interrupt-1\",\"request\":{\"subtype\":\"interrupt\"}}\n")
            .await
            .unwrap();
        let terminal = next_phase(&mut rx, "terminated").await;
        assert_eq!(terminal["resume_allowed"], false);
        assert_eq!(inner.dropped.load(Ordering::SeqCst), 1);
    };
    let operation =
        async { drive(Some(&access), collect(&*provider), &mut reader, &mut writer).await };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(operation, controller)
    })
    .await
    .unwrap();
    assert!(matches!(
        result.unwrap().last(),
        Some(GenerateEvent::Cancelled)
    ));
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn late_pause_drops_an_accepted_stream_before_its_buffered_first_event() {
    let (provider, access, inner) = fixture(
        vec![
            Reply::Events(vec![GenerateEvent::ProviderFailure(activation())]),
            events(),
        ],
        Arc::new(|| true),
    );
    let (_client, mut reader, mut writer, _) = io();
    let mut stream = drive(
        Some(&access),
        provider.generate(&[], &[], &GenerateConfig::default()),
        &mut reader,
        &mut writer,
    )
    .await
    .unwrap();
    let control: EcsTrialControl = {
        let state = access.state();
        serde_json::from_value(
            json!({"run_id":state.run_id,"flow_id":state.flow_id,"revision":2,"action":"pause"}),
        )
        .unwrap()
    };
    access.control(control);
    assert!(matches!(
        stream.next().await,
        Some(GenerateEvent::Cancelled)
    ));
    assert_eq!(inner.dropped.load(Ordering::SeqCst), 2);
    let state = access.state();
    let progress = state.progress.as_ref().unwrap();
    assert_eq!(progress.phase, "paused");
    assert_eq!(progress.dispatch_state, "unknown");
    assert!(progress.local_terminated);
    assert!(!progress.resume_allowed);
}

#[tokio::test(start_paused = true)]
async fn pause_during_a_poll_http_is_unknown_and_cannot_resume() {
    let (provider, access, inner) = fixture(
        vec![
            Reply::Events(vec![GenerateEvent::ProviderFailure(activation())]),
            Reply::Pending,
            events(),
        ],
        Arc::new(|| true),
    );
    let (mut client, mut reader, mut writer, mut rx) = io();
    let controller = async {
        let waiting = next_phase(&mut rx, "waiting_activation").await;
        tokio::time::sleep(Duration::from_secs(6)).await;
        client
            .write_all(control(&waiting, 2, "pause").as_bytes())
            .await
            .unwrap();
        let paused = next_phase(&mut rx, "paused").await;
        assert_eq!(paused["dispatch_state"], "unknown");
        assert_eq!(paused["local_terminated"], true);
        assert_eq!(paused["resume_allowed"], false);
        assert_eq!(inner.dropped.load(Ordering::SeqCst), 2);
        client
            .write_all(control(&waiting, 3, "resume").as_bytes())
            .await
            .unwrap();
    };
    let operation =
        async { drive(Some(&access), collect(&*provider), &mut reader, &mut writer).await };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(100), async {
        tokio::join!(operation, controller)
    })
    .await
    .unwrap();
    assert!(result.is_err());
    assert!(collect(&*provider).await.is_err());
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn eof_terminates_the_actual_silent_stream() {
    let (provider, access, inner) = fixture(
        vec![Reply::Tail(Duration::ZERO, Duration::from_secs(1000))],
        Arc::new(|| true),
    );
    let (client, mut reader, mut writer, _) = io();
    let eof = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        drop(client);
    };
    let operation =
        async { drive(Some(&access), collect(&*provider), &mut reader, &mut writer).await };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(operation, eof)
    })
    .await
    .unwrap();
    assert!(matches!(
        result.unwrap().last(),
        Some(GenerateEvent::Cancelled)
    ));
    assert_eq!(inner.dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn a_later_model_step_gets_its_own_admission_budget_and_preference_check() {
    let checks = Arc::new(AtomicUsize::new(0));
    let count = checks.clone();
    let (provider, access, inner) = fixture(
        vec![
            Reply::Tail(Duration::from_secs(599), Duration::from_secs(2)),
            events(),
        ],
        Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            true
        }),
    );
    let (_client, mut reader, mut writer, _) = io();
    drive(Some(&access), collect(&*provider), &mut reader, &mut writer)
        .await
        .unwrap();
    let first_flow = access.state().flow_id.clone();
    drive(Some(&access), collect(&*provider), &mut reader, &mut writer)
        .await
        .unwrap();
    assert_ne!(first_flow, access.state().flow_id);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    assert_eq!(checks.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn an_already_queued_interrupt_prevents_the_first_http() {
    let (provider, access, inner) = fixture(vec![events()], Arc::new(|| true));
    let (mut client, mut reader, mut writer, _) = io();
    client.write_all(b"{\"type\":\"control_request\",\"request_id\":\"int\",\"request\":{\"subtype\":\"interrupt\"}}\n").await.unwrap();
    assert!(
        drive(Some(&access), collect(&*provider), &mut reader, &mut writer)
            .await
            .is_err()
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn shutdown_is_retained_for_the_outer_reader_owner() {
    let (_, access, _) = fixture(vec![], Arc::new(|| true));
    let (mut client, mut reader, mut writer, _) = io();
    let line="{\"type\":\"control_request\",\"request_id\":\"stop\",\"request\":{\"subtype\":\"shutdown\"}}";
    client
        .write_all(format!("{line}\n").as_bytes())
        .await
        .unwrap();
    drive(
        Some(&access),
        tokio::time::sleep(Duration::from_secs(1)),
        &mut reader,
        &mut writer,
    )
    .await;
    assert_eq!(access.drain_input(), vec![line.to_owned()]);
}

#[tokio::test(start_paused = true)]
async fn credits_preparing_uses_the_bounded_wait_path() {
    let failure = ProviderFailure {
        code: "Sysom.CreditsPreparing".into(),
        kind: ProviderFailureKind::CreditsPreparing,
        ..activation()
    };
    let (provider, access, inner) = fixture(
        vec![
            Reply::Events(vec![GenerateEvent::ProviderFailure(failure)]),
            events(),
        ],
        Arc::new(|| true),
    );
    let (_client, mut reader, mut writer, _) = io();
    drive(Some(&access), collect(&*provider), &mut reader, &mut writer)
        .await
        .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    assert!(String::from_utf8(writer.bytes)
        .unwrap()
        .contains("credits_preparing"));
}

#[tokio::test(start_paused = true)]
async fn buffered_input_has_a_finite_message_limit() {
    let (provider, access, inner) = fixture(vec![events()], Arc::new(|| true));
    let (mut client, mut reader, mut writer, _) = io();
    for _ in 0..129 {
        client
            .write_all(b"{\"type\":\"user\",\"message\":\"queued\"}\n")
            .await
            .unwrap();
    }
    assert!(
        drive(Some(&access), collect(&*provider), &mut reader, &mut writer)
            .await
            .is_err()
    );
    assert_eq!(access.drain_input().len(), 128);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn consumer_drop_records_that_the_http_stream_has_ended() {
    let (provider, access, inner) = fixture(
        vec![Reply::Tail(Duration::ZERO, Duration::from_secs(1000))],
        Arc::new(|| true),
    );
    let stream = provider
        .generate(&[], &[], &GenerateConfig::default())
        .await
        .unwrap();
    drop(stream);
    assert_eq!(inner.dropped.load(Ordering::SeqCst), 1);
    assert!(!access.state().active);
    let control = {
        let state = access.state();
        serde_json::from_value(
            json!({"run_id":state.run_id,"flow_id":state.flow_id,"revision":2,"action":"pause"}),
        )
        .unwrap()
    };
    access.control(control);
    assert!(access.state().progress.as_ref().unwrap().local_terminated);
}

#[tokio::test(start_paused = true)]
async fn cancelling_then_dropping_without_polling_still_acknowledges_actual_drop() {
    let (provider, access, inner) = fixture(
        vec![Reply::Tail(Duration::ZERO, Duration::from_secs(1000))],
        Arc::new(|| true),
    );
    let stream = provider
        .generate(&[], &[], &GenerateConfig::default())
        .await
        .unwrap();
    provider.cancel();
    drop(stream);
    assert_eq!(inner.dropped.load(Ordering::SeqCst), 1);
    let state = access.state();
    let progress = state
        .progress
        .as_ref()
        .expect("no local termination acknowledgement");
    assert_eq!(progress.phase, "terminated");
    assert!(progress.local_terminated);
}

#[tokio::test(start_paused = true)]
async fn finished_stream_checkpoint_consumes_cancel_and_acknowledges_before_tools() {
    let (provider, access, inner) = fixture(vec![events(), events()], Arc::new(|| true));
    let (mut client, mut reader, mut writer, _) = io();
    drive(Some(&access), collect(&*provider), &mut reader, &mut writer)
        .await
        .unwrap();
    client.write_all(b"{\"type\":\"control_request\",\"request_id\":\"interrupt-1\",\"request\":{\"subtype\":\"interrupt\"}}\n").await.unwrap();
    drive(
        Some(&access),
        std::future::ready(()),
        &mut reader,
        &mut writer,
    )
    .await;
    assert!(access.stopped());
    {
        let state = access.state();
        let progress = state
            .progress
            .as_ref()
            .expect("no finished-step cancellation acknowledgement");
        assert_eq!(progress.phase, "terminated");
        assert!(progress.local_terminated);
    }
    assert!(collect(&*provider).await.is_err());
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn quota_remains_a_typed_failure_stream_for_summary_callers() {
    let failure = ProviderFailure {
        code: "Sysom.CreditsExhausted".into(),
        kind: ProviderFailureKind::QuotaExhausted,
        ..activation()
    };
    let (provider, access, inner) = fixture(
        vec![
            Reply::Events(vec![GenerateEvent::ProviderFailure(failure.clone())]),
            events(),
        ],
        Arc::new(|| true),
    );
    let mut stream = provider
        .generate(&[], &[], &GenerateConfig::default())
        .await
        .expect("typed provider failure was flattened into a string");
    assert!(
        matches!(stream.next().await,Some(GenerateEvent::ProviderFailure(value)) if value==failure)
    );
    assert!(stream.next().await.is_none());
    assert_eq!(access.failure(), Some(failure));
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn headers_at_599_allow_first_token_at_605_and_completion_at_610() {
    let (provider, access, inner) = fixture(
        vec![
            Reply::Events(vec![GenerateEvent::ProviderFailure(activation())]),
            Reply::AcceptedTail(
                Duration::from_secs(594),
                Duration::from_secs(6),
                Duration::from_secs(5),
                None,
            ),
        ],
        Arc::new(|| true),
    );
    let (_client, mut reader, mut writer, mut rx) = io();
    let started = tokio::time::Instant::now();
    let operation =
        async { drive(Some(&access), collect(&*provider), &mut reader, &mut writer).await };
    let observe_ready = async {
        let ready = next_phase(&mut rx, "ready").await;
        assert_eq!(started.elapsed(), Duration::from_secs(605));
        assert_eq!(ready["dispatch_state"], "accepted");
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(620), async {
        tokio::join!(operation, observe_ready)
    })
    .await
    .unwrap();
    let events = result.unwrap();
    assert!(matches!(events.first(),Some(GenerateEvent::TextDelta(text)) if text=="after-headers"));
    assert!(!events
        .iter()
        .any(|event| matches!(event, GenerateEvent::RequestAccepted)));
    assert!(matches!(events.last(), Some(GenerateEvent::MessageEnd)));
    assert_eq!(started.elapsed(), Duration::from_secs(610));
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn sse_failure_after_accepted_headers_is_never_replayed() {
    let (provider, access, inner) = fixture(
        vec![
            Reply::AcceptedTail(
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
                Some(activation()),
            ),
            events(),
        ],
        Arc::new(|| true),
    );
    let (_client, mut reader, mut writer, _) = io();
    assert!(
        drive(Some(&access), collect(&*provider), &mut reader, &mut writer)
            .await
            .is_err()
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!(access.failure().unwrap().dispatch, DispatchState::Accepted);
}
