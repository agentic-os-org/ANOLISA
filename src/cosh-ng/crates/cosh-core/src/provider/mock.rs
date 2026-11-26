use async_trait::async_trait;
use futures::stream;

use super::{
    ContentGenerator, GenerateConfig, GenerateEvent, GenerateStream, Message, ToolDeclaration,
};

pub struct MockProvider {
    pub responses: Vec<Vec<GenerateEvent>>,
    call_index: std::sync::atomic::AtomicUsize,
    echo_history: bool,
    repeat_text: Option<String>,
}

impl MockProvider {
    pub fn new(responses: Vec<Vec<GenerateEvent>>) -> Self {
        Self {
            responses,
            call_index: std::sync::atomic::AtomicUsize::new(0),
            echo_history: false,
            repeat_text: None,
        }
    }

    pub fn text_only(text: &str) -> Self {
        Self::new(vec![vec![
            GenerateEvent::TextDelta(text.to_string()),
            GenerateEvent::MessageEnd,
        ]])
    }

    /// Returns the same short text for every request, without exhausting.
    ///
    /// Compaction tests need one provider that can serve both Agent turns
    /// and summarizer calls deterministically.
    pub fn repeat_text(text: &str) -> Self {
        Self {
            responses: Vec::new(),
            call_index: std::sync::atomic::AtomicUsize::new(0),
            echo_history: false,
            repeat_text: Some(text.to_string()),
        }
    }

    pub fn with_tool_call(tool_name: &str, tool_id: &str, arguments: &str) -> Self {
        Self::new(vec![vec![
            GenerateEvent::TextDelta("Let me help.".to_string()),
            GenerateEvent::ToolCallStart {
                index: 0,
                id: tool_id.to_string(),
                name: tool_name.to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: arguments.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ]])
    }

    pub fn history_echo() -> Self {
        Self {
            responses: Vec::new(),
            call_index: std::sync::atomic::AtomicUsize::new(0),
            echo_history: true,
            repeat_text: None,
        }
    }

    /// Emits one representative tool-dispatch turn: reasoning text deltas,
    /// a shell tool call, then message end. Exercises both text and tool-call
    /// projection for fixed event-stream cost measurement.
    pub fn tool_turn() -> Self {
        Self::new(vec![
            vec![
                GenerateEvent::TextDelta("Let me check the disk usage. ".to_string()),
                GenerateEvent::TextDelta("I will run df. ".to_string()),
                GenerateEvent::TextDelta("Reading the result next.".to_string()),
                GenerateEvent::ToolCallStart {
                    index: 0,
                    id: "call-1".to_string(),
                    name: "shell".to_string(),
                },
                GenerateEvent::ToolCallDelta {
                    index: 0,
                    arguments_delta: r#"{"command":"df -h"}"#.to_string(),
                },
                GenerateEvent::ToolCallEnd { index: 0 },
                GenerateEvent::MessageEnd,
            ],
            vec![
                GenerateEvent::TextDelta("The disk check completed.".to_string()),
                GenerateEvent::MessageEnd,
            ],
        ])
    }

    /// Emits a read_file tool turn that completes on both the ACP and the
    /// internal entry without foreground handoff, for representative-load
    /// cost measurement. The path is relative to the session cwd.
    pub fn tool_read() -> Self {
        Self::new(vec![
            vec![
                GenerateEvent::TextDelta("Let me read the file. ".to_string()),
                GenerateEvent::ToolCallStart {
                    index: 0,
                    id: "call-1".to_string(),
                    name: "read_file".to_string(),
                },
                GenerateEvent::ToolCallDelta {
                    index: 0,
                    arguments_delta: r#"{"path":"probe.txt"}"#.to_string(),
                },
                GenerateEvent::ToolCallEnd { index: 0 },
                GenerateEvent::MessageEnd,
            ],
            vec![
                GenerateEvent::TextDelta("The file was read.".to_string()),
                GenerateEvent::MessageEnd,
            ],
        ])
    }

    /// Emits two read_file tool calls in one turn for a multi-tool cost sample.
    /// Both are read-only so no permission request is needed on either entry.
    pub fn tool_multi() -> Self {
        Self::new(vec![
            vec![
                GenerateEvent::TextDelta("Let me read both files. ".to_string()),
                GenerateEvent::ToolCallStart {
                    index: 0,
                    id: "call-1".to_string(),
                    name: "read_file".to_string(),
                },
                GenerateEvent::ToolCallDelta {
                    index: 0,
                    arguments_delta: r#"{"path":"probe.txt"}"#.to_string(),
                },
                GenerateEvent::ToolCallEnd { index: 0 },
                GenerateEvent::ToolCallStart {
                    index: 1,
                    id: "call-2".to_string(),
                    name: "read_file".to_string(),
                },
                GenerateEvent::ToolCallDelta {
                    index: 1,
                    arguments_delta: r#"{"path":"probe2.txt"}"#.to_string(),
                },
                GenerateEvent::ToolCallEnd { index: 1 },
                GenerateEvent::MessageEnd,
            ],
            vec![
                GenerateEvent::TextDelta("Both files were read.".to_string()),
                GenerateEvent::MessageEnd,
            ],
        ])
    }

    /// Emits a write_file tool turn so the standard permission request path
    /// can be exercised on both entries without foreground handoff.
    pub fn tool_write() -> Self {
        Self::new(vec![
            vec![
                GenerateEvent::TextDelta("Let me write the file. ".to_string()),
                GenerateEvent::ToolCallStart {
                    index: 0,
                    id: "call-1".to_string(),
                    name: "write_file".to_string(),
                },
                GenerateEvent::ToolCallDelta {
                    index: 0,
                    arguments_delta: r#"{"path":"out.txt","content":"written by mock"}"#
                        .to_string(),
                },
                GenerateEvent::ToolCallEnd { index: 0 },
                GenerateEvent::MessageEnd,
            ],
            vec![
                GenerateEvent::TextDelta("The file was written.".to_string()),
                GenerateEvent::MessageEnd,
            ],
        ])
    }

    /// Emits a four-step mixed read/write task in one turn: write, verify by
    /// reading back, write a second file, verify again, then finish. The two
    /// writes exercise the standard permission path twice; the reads depend on
    /// the preceding writes, for complex-task cost measurement.
    pub fn task_mix() -> Self {
        Self::new(vec![
            vec![
                GenerateEvent::TextDelta("Let me create the report. ".to_string()),
                GenerateEvent::ToolCallStart {
                    index: 0,
                    id: "call-1".to_string(),
                    name: "write_file".to_string(),
                },
                GenerateEvent::ToolCallDelta {
                    index: 0,
                    arguments_delta: r#"{"path":"report.txt","content":"step one"}"#.to_string(),
                },
                GenerateEvent::ToolCallEnd { index: 0 },
                GenerateEvent::MessageEnd,
            ],
            vec![
                GenerateEvent::TextDelta("Now I will verify it. ".to_string()),
                GenerateEvent::ToolCallStart {
                    index: 0,
                    id: "call-2".to_string(),
                    name: "read_file".to_string(),
                },
                GenerateEvent::ToolCallDelta {
                    index: 0,
                    arguments_delta: r#"{"path":"report.txt"}"#.to_string(),
                },
                GenerateEvent::ToolCallEnd { index: 0 },
                GenerateEvent::MessageEnd,
            ],
            vec![
                GenerateEvent::TextDelta("I will extend it. ".to_string()),
                GenerateEvent::ToolCallStart {
                    index: 0,
                    id: "call-3".to_string(),
                    name: "write_file".to_string(),
                },
                GenerateEvent::ToolCallDelta {
                    index: 0,
                    arguments_delta: r#"{"path":"report2.txt","content":"step two"}"#.to_string(),
                },
                GenerateEvent::ToolCallEnd { index: 0 },
                GenerateEvent::MessageEnd,
            ],
            vec![
                GenerateEvent::TextDelta("Checking the second file. ".to_string()),
                GenerateEvent::ToolCallStart {
                    index: 0,
                    id: "call-4".to_string(),
                    name: "read_file".to_string(),
                },
                GenerateEvent::ToolCallDelta {
                    index: 0,
                    arguments_delta: r#"{"path":"report2.txt"}"#.to_string(),
                },
                GenerateEvent::ToolCallEnd { index: 0 },
                GenerateEvent::MessageEnd,
            ],
            vec![
                GenerateEvent::TextDelta("The task completed.".to_string()),
                GenerateEvent::MessageEnd,
            ],
        ])
    }

    /// Emits many small text deltas in one turn, for fixed event-stream stress.
    pub fn flood(chunk_count: usize, chunk_text: &str) -> Self {
        let mut events = Vec::with_capacity(chunk_count + 1);
        for _ in 0..chunk_count {
            events.push(GenerateEvent::TextDelta(chunk_text.to_string()));
        }
        events.push(GenerateEvent::MessageEnd);
        Self::new(vec![events])
    }

    pub fn partial_error() -> Self {
        Self::new(vec![vec![
            GenerateEvent::TextDelta("partial response".to_string()),
            GenerateEvent::Error("recoverable mock provider error".to_string()),
        ]])
    }
}

#[async_trait]
impl ContentGenerator for MockProvider {
    async fn generate(
        &self,
        messages: &[Message],
        _tools: &[ToolDeclaration],
        _config: &GenerateConfig,
    ) -> Result<GenerateStream, String> {
        if let Some(text) = &self.repeat_text {
            return Ok(Box::pin(stream::iter(vec![
                GenerateEvent::TextDelta(text.clone()),
                GenerateEvent::MessageEnd,
            ])));
        }
        if self.echo_history {
            let history = messages
                .iter()
                .filter(|message| message.role == "user")
                .map(|message| message.content.as_text())
                .collect::<Vec<_>>()
                .join(" | ");
            return Ok(Box::pin(stream::iter(vec![
                GenerateEvent::TextDelta(format!("mock history: {history}")),
                GenerateEvent::MessageEnd,
            ])));
        }
        let idx = self
            .call_index
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let events =
            self.responses.get(idx).cloned().unwrap_or_else(|| {
                vec![GenerateEvent::Error("no more mock responses".to_string())]
            });
        Ok(Box::pin(stream::iter(events)))
    }

    fn cancel(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn mock_provider_text_only() {
        let provider = MockProvider::text_only("Hello!");
        let stream = provider
            .generate(&[], &[], &GenerateConfig::default())
            .await
            .unwrap();
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], GenerateEvent::TextDelta(t) if t == "Hello!"));
        assert!(matches!(&events[1], GenerateEvent::MessageEnd));
    }

    #[tokio::test]
    async fn mock_provider_with_tool_call() {
        let provider = MockProvider::with_tool_call("shell", "call-1", r#"{"command":"ls"}"#);
        let stream = provider
            .generate(&[], &[], &GenerateConfig::default())
            .await
            .unwrap();
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 5);
        assert!(matches!(&events[1], GenerateEvent::ToolCallStart { name, .. } if name == "shell"));
    }

    #[tokio::test]
    async fn mock_provider_tool_turn_emits_text_and_a_shell_tool_call() {
        let provider = MockProvider::tool_turn();
        let stream = provider
            .generate(&[], &[], &GenerateConfig::default())
            .await
            .unwrap();
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 7);
        assert!(matches!(&events[0], GenerateEvent::TextDelta(_)));
        assert!(matches!(&events[3], GenerateEvent::ToolCallStart { name, .. } if name == "shell"));
        assert!(matches!(&events[4], GenerateEvent::ToolCallDelta { .. }));
        assert!(matches!(
            &events[5],
            GenerateEvent::ToolCallEnd { index: 0 }
        ));
        assert!(matches!(&events[6], GenerateEvent::MessageEnd));
        let second = provider
            .generate(&[], &[], &GenerateConfig::default())
            .await
            .unwrap();
        let events2: Vec<_> = second.collect().await;
        assert_eq!(events2.len(), 2);
        assert!(matches!(&events2[1], GenerateEvent::MessageEnd));
    }

    #[tokio::test]
    async fn mock_provider_tool_read_emits_a_read_file_tool_call() {
        let provider = MockProvider::tool_read();
        let stream = provider
            .generate(&[], &[], &GenerateConfig::default())
            .await
            .unwrap();
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 5);
        assert!(
            matches!(&events[1], GenerateEvent::ToolCallStart { name, .. } if name == "read_file")
        );
        assert!(
            matches!(&events[2], GenerateEvent::ToolCallDelta { arguments_delta, .. } if arguments_delta.contains("probe.txt"))
        );
        assert!(matches!(
            &events[3],
            GenerateEvent::ToolCallEnd { index: 0 }
        ));
        assert!(matches!(&events[4], GenerateEvent::MessageEnd));
        let second = provider
            .generate(&[], &[], &GenerateConfig::default())
            .await
            .unwrap();
        let events2: Vec<_> = second.collect().await;
        assert_eq!(events2.len(), 2);
        assert!(matches!(&events2[1], GenerateEvent::MessageEnd));
    }

    #[tokio::test]
    async fn mock_provider_tool_multi_emits_two_read_file_tool_calls() {
        let provider = MockProvider::tool_multi();
        let stream = provider
            .generate(&[], &[], &GenerateConfig::default())
            .await
            .unwrap();
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 8);
        assert!(
            matches!(&events[1], GenerateEvent::ToolCallStart { name, id, .. } if name == "read_file" && id == "call-1")
        );
        assert!(
            matches!(&events[4], GenerateEvent::ToolCallStart { name, id, .. } if name == "read_file" && id == "call-2")
        );
        assert!(matches!(&events[7], GenerateEvent::MessageEnd));
        let second = provider
            .generate(&[], &[], &GenerateConfig::default())
            .await
            .unwrap();
        let events2: Vec<_> = second.collect().await;
        assert_eq!(events2.len(), 2);
        assert!(matches!(&events2[1], GenerateEvent::MessageEnd));
    }

    #[tokio::test]
    async fn mock_provider_tool_write_emits_a_write_file_tool_call() {
        let provider = MockProvider::tool_write();
        let stream = provider
            .generate(&[], &[], &GenerateConfig::default())
            .await
            .unwrap();
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 5);
        assert!(
            matches!(&events[1], GenerateEvent::ToolCallStart { name, .. } if name == "write_file")
        );
        assert!(
            matches!(&events[2], GenerateEvent::ToolCallDelta { arguments_delta, .. } if arguments_delta.contains("out.txt"))
        );
        assert!(matches!(
            &events[3],
            GenerateEvent::ToolCallEnd { index: 0 }
        ));
        assert!(matches!(&events[4], GenerateEvent::MessageEnd));
        let second = provider
            .generate(&[], &[], &GenerateConfig::default())
            .await
            .unwrap();
        let events2: Vec<_> = second.collect().await;
        assert_eq!(events2.len(), 2);
        assert!(matches!(&events2[1], GenerateEvent::MessageEnd));
    }

    #[tokio::test]
    async fn mock_provider_task_mix_emits_write_read_write_read_then_finish() {
        let provider = MockProvider::task_mix();
        let expected = [
            ("write_file", "call-1", "report.txt"),
            ("read_file", "call-2", "report.txt"),
            ("write_file", "call-3", "report2.txt"),
            ("read_file", "call-4", "report2.txt"),
        ];
        for (name, id, path) in expected {
            let stream = provider
                .generate(&[], &[], &GenerateConfig::default())
                .await
                .unwrap();
            let events: Vec<_> = stream.collect().await;
            assert_eq!(events.len(), 5);
            assert!(matches!(&events[0], GenerateEvent::TextDelta(_)));
            assert!(
                matches!(&events[1], GenerateEvent::ToolCallStart { name: n, id: i, .. } if n == name && i == id)
            );
            assert!(
                matches!(&events[2], GenerateEvent::ToolCallDelta { arguments_delta, .. } if arguments_delta.contains(path))
            );
            assert!(matches!(
                &events[3],
                GenerateEvent::ToolCallEnd { index: 0 }
            ));
            assert!(matches!(&events[4], GenerateEvent::MessageEnd));
        }
        let last = provider
            .generate(&[], &[], &GenerateConfig::default())
            .await
            .unwrap();
        let events: Vec<_> = last.collect().await;
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], GenerateEvent::TextDelta(t) if t == "The task completed."));
        assert!(matches!(&events[1], GenerateEvent::MessageEnd));
    }

    #[tokio::test]
    async fn mock_provider_flood_emits_one_delta_per_chunk() {
        let provider = MockProvider::flood(4096, "0123456789abcdefghij");
        let stream = provider
            .generate(&[], &[], &GenerateConfig::default())
            .await
            .unwrap();
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 4097);
        assert!(events.iter().take(4096).all(|event| matches!(
            event,
            GenerateEvent::TextDelta(text) if text == "0123456789abcdefghij"
        )));
        assert!(matches!(&events[4096], GenerateEvent::MessageEnd));
    }

    #[tokio::test]
    async fn mock_provider_multi_turn() {
        let provider = MockProvider::new(vec![
            vec![
                GenerateEvent::TextDelta("first".to_string()),
                GenerateEvent::MessageEnd,
            ],
            vec![
                GenerateEvent::TextDelta("second".to_string()),
                GenerateEvent::MessageEnd,
            ],
        ]);
        let s1 = provider
            .generate(&[], &[], &GenerateConfig::default())
            .await
            .unwrap();
        let e1: Vec<_> = s1.collect().await;
        assert!(matches!(&e1[0], GenerateEvent::TextDelta(t) if t == "first"));

        let s2 = provider
            .generate(&[], &[], &GenerateConfig::default())
            .await
            .unwrap();
        let e2: Vec<_> = s2.collect().await;
        assert!(matches!(&e2[0], GenerateEvent::TextDelta(t) if t == "second"));
    }
}
