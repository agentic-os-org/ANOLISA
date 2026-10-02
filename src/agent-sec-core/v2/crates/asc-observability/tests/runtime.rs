#![cfg(feature = "runtime")]

use asc_observability::{Context, bind_trace_context_input, init_runtime, parent_span, snapshot};
use std::{process::Command, time::Duration};

#[test]
fn runtime_filters_do_not_disable_context_or_export_arbitrary_messages() {
    if std::env::var_os("ASC_OTEL_TEST_CHILD").is_some() {
        let runtime = init_runtime("asc-runtime-test").unwrap();
        assert!(matches!(
            init_runtime("asc-runtime-test"),
            Err("subscriber_conflict")
        ));
        assert!(std::panic::catch_unwind(|| panic!("DO_NOT_EXPORT_SECRET_PAYLOAD")).is_err());
        std::thread::spawn(|| {
            tracing::warn!(target: "asc_process_diagnostic", "background_storage_warning");
        })
        .join()
        .unwrap();
        let context =
            bind_trace_context_input(&Context::new(), &serde_json::json!({"sessionId":"session"}))
                .unwrap();
        let span = parent_span(
            tracing::info_span!(target: "asc_runtime_test", parent: None, "test.root"),
            context,
        );
        span.in_scope(|| {
            let _anchor = asc_observability::request_context().attach();
            let before = snapshot();
            assert!(before.trace_id.is_some());
            assert!(before.span_id.is_some());
            assert!(
                !opentelemetry::trace::TraceContextExt::span(&Context::current())
                    .span_context()
                    .is_sampled()
            );
            assert_eq!(before.agent["session_id"], "session");
            tracing::error!(target: "asc_runtime_test", "DO_NOT_EXPORT_SECRET_PAYLOAD");
            asc_observability::diagnostic("test_context_readable");
            tracing::info_span!(target: "asc_runtime_test", "test.child").in_scope(|| {
                assert_eq!(snapshot().trace_id, before.trace_id);
                assert_ne!(snapshot().span_id, before.span_id);
                assert_eq!(snapshot().agent, before.agent);
                asc_observability::diagnostic("test_child_context");
            });
        });
        drop(span);
        runtime.shutdown(Duration::from_secs(2));
        return;
    }
    for sampler in ["always_on", "always_off"] {
        for filter in ["off", "info"] {
            let mut command = Command::new(std::env::current_exe().unwrap());
            for (key, _) in std::env::vars_os() {
                if key.to_string_lossy().starts_with("OTEL_") {
                    command.env_remove(key);
                }
            }
            let output = command
                .args([
                    "--exact",
                    "runtime_filters_do_not_disable_context_or_export_arbitrary_messages",
                    "--nocapture",
                ])
                .env("ASC_OTEL_TEST_CHILD", "1")
                .env("OTEL_TRACES_EXPORTER", "otlp")
                .env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:1")
                .env("OTEL_BSP_MAX_QUEUE_SIZE", "invalid")
                .env("OTEL_TRACES_SAMPLER", sampler)
                .env("RUST_LOG", filter)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(!stderr.contains("DO_NOT_EXPORT_SECRET_PAYLOAD"));
            assert!(stderr.contains("runtime: panic"));
            assert!(
                stderr
                    .lines()
                    .any(|line| line == "background_storage_warning")
            );
            assert_eq!(stderr.contains("test_context_readable"), filter == "info");
            if filter == "info" {
                let records: Vec<serde_json::Value> = stderr
                    .lines()
                    .filter(|line| line.starts_with('{'))
                    .map(|line| {
                        let record: serde_json::Value = serde_json::from_str(line).unwrap();
                        serde_json::from_str(record["fields"]["correlation"].as_str().unwrap())
                            .unwrap()
                    })
                    .collect();
                assert_eq!(records.len(), 2);
                assert_eq!(records[0]["trace_id"], records[1]["trace_id"]);
                assert_ne!(records[0]["span_id"], records[1]["span_id"]);
                assert_eq!(records[0]["span_id"], records[1]["request_span_id"]);
            }
        }
    }
}
