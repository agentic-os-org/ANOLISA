use asc_observability::*;
use opentelemetry::{
    baggage::BaggageExt as _,
    trace::{TraceContextExt as _, TracerProvider as _},
};
use opentelemetry_sdk::trace::{InMemorySpanExporter, Sampler, SdkTracerProvider};
use std::{
    collections::HashMap,
    sync::{Arc, Barrier},
    time::Duration,
};
use tracing::{Instrument as _, instrument::WithSubscriber as _};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::layer::SubscriberExt as _;

fn setup(sampler: Sampler) -> (SdkTracerProvider, InMemorySpanExporter, tracing::Dispatch) {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_sampler(sampler)
        .with_span_processor(AgentFieldProcessor)
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry().with(
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("tests"))
            .with_context_activation(true),
    );
    (provider, exporter, tracing::Dispatch::new(subscriber))
}
fn parent(session: &str) -> Context {
    let labels = serde_json::json!({"agentName":"agent", "sessionId":session,"runId":"run","trace_id":"opaque-label"});
    bind_trace_context_input(&Context::new(), &labels).unwrap()
}

#[test]
fn native_children_project_context_restore_and_ignore_sampling() {
    for sampler in [Sampler::AlwaysOn, Sampler::AlwaysOff] {
        let expected_spans = if matches!(sampler, Sampler::AlwaysOn) {
            2
        } else {
            0
        };
        let (provider, exporter, dispatch) = setup(sampler);
        tracing::dispatcher::with_default(&dispatch, || {
            let root = parent_span(tracing::info_span!(parent: None,"root"), parent("session"));
            root.in_scope(|| {
                let _anchor = request_context().attach();
                let before = snapshot();
                assert!(before.trace_id.is_some());
                assert_eq!(
                    tracing::Span::current()
                        .context()
                        .span()
                        .span_context()
                        .trace_id()
                        .to_string(),
                    before.trace_id.clone().unwrap()
                );
                let child = tracing::info_span!("free.function.name");
                child.in_scope(|| {
                    let snap = snapshot();
                    assert_eq!(snap.trace_id, before.trace_id);
                    assert_ne!(snap.span_id, before.span_id);
                    assert_eq!(snap.request_span_id, before.span_id);
                    assert_eq!(snap.agent, before.agent);
                    assert_eq!(snap.compatibility.trace_id.as_deref(), Some("opaque-label"));
                });
                assert_eq!(snapshot(), before);
            });
            assert!(snapshot().trace_id.is_none());
        });
        provider.force_flush().unwrap();
        let finished = exporter.get_finished_spans().unwrap();
        assert_eq!(finished.len(), expected_spans);
        for span in finished {
            assert!(
                span.attributes
                    .iter()
                    .any(|kv| kv.key.as_str() == "agentsec.session.id"
                        && kv.value.to_string() == "session")
            );
        }
    }
}

#[test]
fn remote_parent_baggage_and_bad_content_are_independent() {
    let mut headers = HashMap::from([
        (
            "traceparent".into(),
            "00-11111111111111111111111111111111-2222222222222222-01".into(),
        ),
        ("tracestate".into(), "vendor=value".into()),
        (
            "baggage".into(),
            "agentsec.session.id=session,unknown=secret".into(),
        ),
    ]);
    let context = extract_parent(&headers);
    assert!(context.span().span_context().is_remote());
    assert_eq!(context.baggage().len(), 1);
    assert_eq!(
        context.span().span_context().trace_state().header(),
        "vendor=value"
    );
    let (provider, exporter, dispatch) = setup(Sampler::AlwaysOn);
    tracing::dispatcher::with_default(&dispatch, || {
        parent_span(
            tracing::info_span!(parent: None,"server",otel.kind="server"),
            context,
        )
        .in_scope(|| {
            assert_eq!(
                snapshot().trace_id.as_deref(),
                Some("11111111111111111111111111111111")
            );
        });
    });
    provider.force_flush().unwrap();
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans[0].parent_span_id.to_string(), "2222222222222222");
    assert_ne!(
        spans[0].span_context.span_id().to_string(),
        "2222222222222222"
    );
    headers.insert(
        "traceparent".into(),
        "00-00000000000000000000000000000000-2222222222222222-01".into(),
    );
    assert!(!extract_parent(&headers).span().span_context().is_valid());
    assert_eq!(extract_parent(&headers).baggage().len(), 1);
    for baggage in [
        "agentsec.session.id=a,agentsec.session.id=b",
        "agentsec.session.id=%zz",
        "agentsec.session.id=%ff",
        "agentsec.session.id=a b",
    ] {
        headers.insert("baggage".into(), baggage.into());
        assert!(extract_parent(&headers).baggage().is_empty());
    }
    headers.insert(
        "traceparent".into(),
        "00-11111111111111111111111111111111-2222222222222222-00".into(),
    );
    headers.insert("baggage".into(), "agentsec.session.id=s".into());
    for state in [
        "invalid",
        "Uppercase=value",
        "key=value=invalid",
        &"a".repeat(513),
    ] {
        headers.insert("tracestate".into(), state.into());
        let parent = extract_parent(&headers);
        assert!(parent.span().span_context().is_valid());
        assert!(!parent.span().span_context().is_sampled());
        assert!(
            parent
                .span()
                .span_context()
                .trace_state()
                .header()
                .is_empty(),
            "{state}"
        );
        assert_eq!(parent.baggage().len(), 1);
    }
    for baggage in [
        "a=x,".repeat(32) + "b=y",
        "a=".to_owned() + &"x".repeat(16384),
    ] {
        headers.insert("baggage".into(), baggage);
        let parent = extract_parent(&headers);
        assert!(parent.span().span_context().is_valid());
        assert!(parent.baggage().is_empty());
    }
}

#[test]
fn unicode_roundtrip_and_original_normalization() {
    let long = "🦀".repeat(300);
    assert_eq!(bounded(&long).chars().count(), 256);
    assert_eq!(normalize("\u{1c} hi \u{1f}").as_deref(), Some("hi"));
    let context = bind_trace_context_input(&Context::new(),&serde_json::json!({"session_id":" ","sessionId":"fallback","agent_name":long,"run_id":long,"call_id":long,"tool_call_id":long})).unwrap();
    let mut headers = HashMap::new();
    inject_context(&context, &mut headers);
    assert!(headers["baggage"].len() <= 16384);
    let extracted = extract_parent(&headers);
    for (key, value) in context.baggage() {
        assert_eq!(extracted.baggage().get(key.as_str()), Some(&value.0));
    }
    assert_eq!(
        extracted
            .baggage()
            .get("agentsec.session.id")
            .unwrap()
            .to_string(),
        "fallback"
    );
    let context = bind_metadata(
        &Context::new(),
        &serde_json::json!({"sessionId":" s ","runId":"r","toolCallId":"t"}),
        MetadataKind::ToolCall,
    )
    .unwrap();
    headers.clear();
    inject_context(&context, &mut headers);
    let guard = extract_parent(&headers).attach();
    assert_eq!(
        validate_metadata(MetadataKind::ToolCall).unwrap().agent["session_id"],
        " s "
    );
    drop(guard);
    assert_eq!(
        validate_metadata(MetadataKind::ModelCall).unwrap_err(),
        "session_id"
    );
}

#[test]
fn interleaved_tasks_threads_and_panic_restore_full_context() {
    let (_provider, _exporter, dispatch) = setup(Sampler::AlwaysOff);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let gate = Arc::new(Barrier::new(2));
    runtime.block_on(async {
        let mut jobs = Vec::new();
        for name in ["one", "two"] {
            let gate = gate.clone();
            let dispatch = dispatch.clone();
            let span = tracing::dispatcher::with_default(&dispatch, || {
                parent_span(tracing::info_span!(parent: None,"task"), parent(name))
            });
            let task_dispatch = dispatch.clone();
            jobs.push(tokio::spawn(
                async move {
                    let before = snapshot();
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    assert_eq!(snapshot(), before);
                    let captured = request_context();
                    let child = parent_span(tracing::info_span!(parent: None,"blocking"), captured);
                    tokio::task::spawn_blocking(move || {
                        tracing::dispatcher::with_default(&dispatch, || {
                            child.in_scope(|| {
                                gate.wait();
                                assert_eq!(snapshot().agent["session_id"], name);
                                assert_eq!(
                                    snapshot().compatibility.trace_id.as_deref(),
                                    Some("opaque-label")
                                );
                                assert_eq!(snapshot().trace_id, before.trace_id);
                                assert_eq!(snapshot().request_span_id, before.span_id);
                                assert_eq!(snapshot().agent, before.agent);
                            });
                        });
                    })
                    .await
                    .unwrap();
                }
                .instrument(span)
                .with_subscriber(task_dispatch),
            ));
        }
        for job in jobs {
            job.await.unwrap();
        }
    });
    tracing::dispatcher::with_default(&dispatch, || {
        let outcome = std::panic::catch_unwind(|| {
            parent_span(tracing::info_span!(parent: None,"panic"), parent("panic"))
                .in_scope(|| panic!("canary"))
        });
        assert!(outcome.is_err());
        assert!(snapshot().agent.is_empty());
        assert!(snapshot().trace_id.is_none());
    });
}

#[test]
fn frozen_v1_oracle_matches_trace_context_projection() {
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/tracing/v1-trace-context-normalization.json"
    ))
    .unwrap();
    for case in cases.as_array().unwrap() {
        let guard = bind_trace_context_input(&Context::new(), &case["input"])
            .unwrap()
            .attach();
        let snap = snapshot();
        let mut actual = serde_json::to_value(snap.agent).unwrap();
        if let Some(label) = snap.compatibility.trace_id {
            actual["trace_id"] = label.into();
        }
        assert_eq!(actual, case["expected"]);
        drop(guard);
    }
}

#[test]
fn metadata_matches_v1_schema_without_inheriting_record_fields() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/tracing/metadata.json")).unwrap();
    for sampler in [Sampler::AlwaysOn, Sampler::AlwaysOff] {
        let (provider, exporter, dispatch) = setup(sampler);
        tracing::dispatcher::with_default(&dispatch, || {
            let context = bind_trace_context_input(
                &parent("parent-session"),
                &serde_json::json!({
                    "callId": "parent-call", "toolCallId": "parent-tool"
                }),
            )
            .unwrap();
            parent_span(
                tracing::info_span!(parent: None, "metadata.parent"),
                context,
            )
            .in_scope(|| {
                let _anchor = request_context().attach();
                let before = snapshot();
                for case in cases.as_array().unwrap() {
                    for (name, expected) in case["expected"].as_object().unwrap() {
                        let kind = match name.as_str() {
                            "agent" => MetadataKind::AgentRun,
                            "model" => MetadataKind::ModelCall,
                            "tool" => MetadataKind::ToolCall,
                            _ => unreachable!(),
                        };
                        let bound = bind_metadata(&Context::current(), &case["input"], kind);
                        if expected.is_null() {
                            assert!(bound.is_err(), "{name}: {case}");
                        } else {
                            let bound = bound.unwrap();
                            let mut carrier = HashMap::new();
                            inject_context(&bound, &mut carrier);
                            let _bound = bound.attach();
                            let snap = validate_metadata(kind).unwrap();
                            assert_eq!(snap.trace_id, before.trace_id);
                            assert_eq!(snap.span_id, before.span_id);
                            assert_eq!(snap.request_span_id, before.request_span_id);
                            assert_eq!(snap.compatibility, before.compatibility);
                            let mut fields = snap.agent.clone();
                            assert_eq!(fields.remove("agent_name").as_deref(), Some("agent"));
                            assert_eq!(
                                serde_json::to_value(fields).unwrap(),
                                *expected,
                                "{name}: {case}"
                            );
                            // Native consumers use the same carrier and require no metadata argument.
                            let _remote = extract_parent(&carrier).attach();
                            assert_eq!(validate_metadata(kind).unwrap().agent, snap.agent);
                        }
                        assert_eq!(snapshot(), before, "binding must not mutate its parent");
                    }
                }
            });
        });
        provider.force_flush().unwrap();
        // Snapshot-based validation has identical semantics with zero exported spans.
        assert!(exporter.get_finished_spans().is_ok());
    }
}

#[test]
fn metadata_binding_projects_only_record_values_to_children_and_tasks() {
    let (provider, exporter, dispatch) = setup(Sampler::AlwaysOn);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    tracing::dispatcher::with_default(&dispatch, || {
        let context = bind_trace_context_input(
            &parent("old"),
            &serde_json::json!({"callId":"old-call", "toolCallId":"old-tool"}),
        )
        .unwrap();
        parent_span(tracing::info_span!(parent: None, "metadata.scope"), context).in_scope(|| {
            let _anchor = request_context().attach();
            let before = snapshot();
            let bound = bind_metadata(
                &Context::current(),
                &serde_json::json!({
                    "sessionId":"new", "runId":"new-run", "callId":null
                }),
                MetadataKind::ModelCall,
            )
            .unwrap();
            let _bound = bound.attach();
            let expected = snapshot();
            assert!(!expected.agent.contains_key("call_id"));
            assert!(!expected.agent.contains_key("tool_call_id"));
            assert_eq!(
                validate_metadata(MetadataKind::ToolCall).unwrap_err(),
                "tool_call_id"
            );
            for explicit in [false, true] {
                let child = if explicit {
                    parent_span(
                        tracing::info_span!(parent: None, "metadata.explicit"),
                        Context::current(),
                    )
                } else {
                    tracing::info_span!("metadata.contextual")
                };
                child.in_scope(|| {
                    let snap = snapshot();
                    assert_eq!(snap.agent, expected.agent);
                    assert_eq!(snap.request_span_id, before.span_id);
                    assert_eq!(snap.compatibility, before.compatibility);
                    assert_eq!(snap.trace_id, before.trace_id);
                });
            }
            let child = parent_span(
                tracing::info_span!(parent: None, "metadata.task"),
                Context::current(),
            );
            let task_snapshot = runtime.block_on(async {
                tokio::spawn(
                    async {
                        tokio::task::yield_now().await;
                        validate_metadata(MetadataKind::ModelCall).unwrap()
                    }
                    .instrument(child)
                    .with_subscriber(dispatch.clone()),
                )
                .await
                .unwrap()
            });
            assert_eq!(task_snapshot.agent, expected.agent);
            assert_eq!(task_snapshot.trace_id, expected.trace_id);
            assert_eq!(task_snapshot.request_span_id, expected.request_span_id);
            assert_eq!(task_snapshot.compatibility, expected.compatibility);
        });
    });
    provider.force_flush().unwrap();
    let spans = exporter.get_finished_spans().unwrap();
    let root = spans.iter().find(|s| s.name == "metadata.scope").unwrap();
    for child in spans.iter().filter(|s| s.name != "metadata.scope") {
        assert_eq!(child.parent_span_id, root.span_context.span_id());
        assert!(child.attributes.iter().any(|kv| kv.key.as_str() == "agentsec.session.id" && kv.value.to_string() == "new"));
        assert!(!child.attributes.iter().any(|kv| matches!(
            kv.key.as_str(),
            "agentsec.call.id" | "agentsec.tool_call.id"
        )));
    }
    assert_eq!(spans.len(), 4);
}

#[test]
fn native_metadata_requires_each_hook_field_without_inventing_values() {
    for (baggage, kind, missing) in [
        ("agentsec.run.id=r", MetadataKind::AgentRun, "session_id"),
        ("agentsec.session.id=s", MetadataKind::ModelCall, "run_id"),
        (
            "agentsec.session.id=s,agentsec.run.id=r",
            MetadataKind::ToolCall,
            "tool_call_id",
        ),
    ] {
        let context = extract_parent(&HashMap::from([("baggage".into(), baggage.into())]));
        let _guard = context.attach();
        assert_eq!(validate_metadata(kind).unwrap_err(), missing);
        assert!(!snapshot().agent.contains_key(missing));
    }
}

#[test]
fn aborted_task_and_reused_worker_do_not_retain_context() {
    let (_provider, _exporter, dispatch) = setup(Sampler::AlwaysOff);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (ready, entered) = tokio::sync::oneshot::channel();
        let span = tracing::dispatcher::with_default(&dispatch, || {
            parent_span(
                tracing::info_span!(parent: None, "abort.task"),
                parent("aborted"),
            )
        });
        let job = tokio::spawn(
            async move {
                assert_eq!(snapshot().agent["session_id"], "aborted");
                ready.send(std::thread::current().id()).unwrap();
                std::future::pending::<()>().await;
            }
            .instrument(span)
            .with_subscriber(dispatch.clone()),
        );
        let worker = entered.await.unwrap();
        job.abort();
        assert!(job.await.unwrap_err().is_cancelled());
        let (reused, snap) = tokio::spawn(async { (std::thread::current().id(), snapshot()) })
            .await
            .unwrap();
        assert_eq!(reused, worker);
        assert!(snap.trace_id.is_none());
        assert!(snap.agent.is_empty());
        assert!(snap.request_span_id.is_none());
        assert_eq!(snap.compatibility, CompatibilityCorrelation::default());
        let context = parent("blocking");
        let worker = tokio::task::spawn_blocking(move || {
            let _context = context.attach();
            assert_eq!(snapshot().agent["session_id"], "blocking");
            std::thread::current().id()
        })
        .await
        .unwrap();
        let (reused, snap) =
            tokio::task::spawn_blocking(|| (std::thread::current().id(), snapshot()))
                .await
                .unwrap();
        assert_eq!(reused, worker);
        assert!(snap.agent.is_empty());
        assert_eq!(snap.compatibility, CompatibilityCorrelation::default());
    });
}

#[test]
fn unknown_utf8_does_not_erase_allowed_attribution() {
    for baggage in [
        "agentsec.session.id=s,unknown=%ff",
        "unknown=%ff,agentsec.session.id=s",
    ] {
        let context = extract_parent(&HashMap::from([("baggage".into(), baggage.into())]));
        assert_eq!(context.baggage().len(), 1);
        assert_eq!(
            context
                .baggage()
                .get("agentsec.session.id")
                .unwrap()
                .as_str(),
            "s"
        );
    }
}

#[test]
fn injection_emits_one_member_header_and_preserves_escaped_values() {
    struct CountingCarrier(Vec<(String, String)>);
    impl opentelemetry::propagation::Injector for CountingCarrier {
        fn set(&mut self, key: &str, value: String) {
            self.0.push((key.into(), value));
        }
    }
    let value = " a-b_中% ;,=\\\"\n ";
    let context =
        Context::new().with_baggage([opentelemetry::KeyValue::new("agentsec.session.id", value)]);
    let mut carrier = CountingCarrier(Vec::new());
    inject_context(&context, &mut carrier);
    assert_eq!(carrier.0.len(), 1);
    assert_eq!(carrier.0[0].0, "baggage");
    assert!(carrier.0[0].1.contains("a-b_"));
    let headers: HashMap<_, _> = carrier.0.into_iter().collect();
    assert_eq!(
        extract_parent(&headers)
            .baggage()
            .get("agentsec.session.id")
            .unwrap()
            .as_str(),
        value
    );
}

#[test]
fn sdk_interoperability_is_limited_by_encoded_wire_size() {
    use opentelemetry::propagation::TextMapPropagator as _;
    let keys = [
        "agentsec.agent.name",
        "agentsec.session.id",
        "agentsec.run.id",
        "agentsec.call.id",
        "agentsec.tool_call.id",
    ];
    for count in [64, 256] {
        let context = Context::new()
            .with_baggage(keys.map(|key| opentelemetry::KeyValue::new(key, "中".repeat(count))));
        let mut headers = HashMap::new();
        inject_context(&context, &mut headers);
        assert_eq!(extract_parent(&headers).baggage().len(), 5);
        let sdk = opentelemetry_sdk::propagation::BaggagePropagator::new().extract(&headers);
        if count == 64 {
            assert!(headers["baggage"].len() <= 8192);
            for key in keys {
                assert_eq!(sdk.baggage().get(key), context.baggage().get(key));
            }
        } else {
            assert!(headers["baggage"].len() > 8192);
            assert!(headers["baggage"].len() <= 16384);
            // The locked SDK receiver drops this header. AgentSec's 16 KiB
            // adapter preserves it; shrinking V1 values silently is not allowed.
            assert!(sdk.baggage().is_empty());
        }
    }
}
