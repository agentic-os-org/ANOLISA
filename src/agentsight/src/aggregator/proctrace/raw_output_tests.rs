use super::super::process::MAX_RETAINED_PROCESS_OUTPUT_BYTES as CAP;
use super::*;
use crate::chrome_trace::ToChromeTraceEvent;
use crate::probes::proctrace::ProcEventHeader;

fn header(timestamp: u64) -> ProcEventHeader {
    // SAFETY: the generated C header consists only of integers and byte arrays.
    let mut header: ProcEventHeader = unsafe { std::mem::zeroed() };
    header.timestamp_ns = timestamp;
    header
}

#[test]
fn raw_output_cap_preserves_utf8_through_trace_serialization() {
    for fd in [1, 2] {
        for prefix_len in [CAP - 2, CAP - 1] {
            let mut agg = ProcessEventAggregator::new();
            agg.process_event(&VariableEvent::Exec {
                header: header(1),
                filename: "probe".into(),
                args: "probe".into(),
            });
            // Real capture events are bounded; do not rely on an oversized chunk.
            for chunk in vec![b'a'; prefix_len].chunks(4096) {
                agg.process_event(&VariableEvent::Stdout {
                    header: header(2),
                    fd,
                    payload: chunk.to_vec(),
                });
            }
            agg.process_event(&VariableEvent::Stdout {
                header: header(3),
                fd,
                payload: vec![0xe4],
            });
            agg.process_event(&VariableEvent::Stdout {
                header: header(4),
                fd,
                payload: vec![0xb8, 0xad, b'Z'],
            });
            // A later ASCII event must not fill the room left by UTF-8 backoff.
            agg.process_event(&VariableEvent::Stdout {
                header: header(5),
                fd,
                payload: b"later".to_vec(),
            });
            // The other stream remains independent of this stream's cap.
            agg.process_event(&VariableEvent::Stdout {
                header: header(6),
                fd: if fd == 1 { 2 } else { 1 },
                payload: b"other".to_vec(),
            });
            let proc = agg
                .process_event(&VariableEvent::Exit {
                    header: header(7),
                    exit_code: 0,
                })
                .unwrap();
            let (bytes, other, field) = if fd == 1 {
                (&proc.stdout_data, &proc.stderr_data, "stdout")
            } else {
                (&proc.stderr_data, &proc.stdout_data, "stderr")
            };
            assert_eq!(
                bytes,
                &vec![b'a'; prefix_len],
                "fd={fd}, prefix={prefix_len}"
            );
            assert_eq!(other, b"other");
            assert_eq!(proc.end_timestamp_ns, 7);
            let events = proc.to_chrome_trace_events();
            let event = events
                .iter()
                .find(|e| e.cat == "process_lifecycle")
                .unwrap();
            let json: serde_json::Value =
                serde_json::from_str(&serde_json::to_string(event).unwrap()).unwrap();
            assert_eq!(json["args"][field], "a".repeat(prefix_len));
            assert!(proc.is_complete);
        }
    }
}

#[test]
fn raw_split_character_below_cap_is_reassembled() {
    let mut agg = ProcessEventAggregator::new();
    agg.process_event(&VariableEvent::Exec {
        header: header(1),
        filename: "probe".into(),
        args: "probe".into(),
    });
    for (timestamp, payload) in [(2, vec![0xe4]), (3, vec![0xb8]), (4, vec![0xad])] {
        agg.process_event(&VariableEvent::Stdout {
            header: header(timestamp),
            fd: 1,
            payload,
        });
    }
    let proc = agg
        .process_event(&VariableEvent::Exit {
            header: header(5),
            exit_code: 0,
        })
        .unwrap();
    assert_eq!(proc.stdout_string(), "中");
}
