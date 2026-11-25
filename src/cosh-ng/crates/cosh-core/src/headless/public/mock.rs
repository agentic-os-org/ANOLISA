use crate::config::CoreConfig;
use crate::provider::mock::MockProvider;
use crate::provider::{ContentGenerator, GenerateEvent};

pub(super) fn create_provider(config: &CoreConfig) -> Option<Box<dyn ContentGenerator>> {
    let resolved = config.resolve_provider();
    if resolved.provider_type != "mock" {
        return None;
    }
    let (tool, id, arguments) = match resolved.model.as_str() {
        "mock-public-write" => (
            "write_file",
            "public-write-1",
            r#"{"path":"headless-note.txt","content":"fixed by public headless\n"}"#,
        ),
        "mock-public-escape" => (
            "write_file",
            "public-escape-1",
            r#"{"path":"../escape-marker","content":"must not exist"}"#,
        ),
        "mock-public-symlink" => (
            "write_file",
            "public-symlink-1",
            r#"{"path":"link/escape-marker","content":"must not exist"}"#,
        ),
        "mock-public-command-failure" => (
            "run_command",
            "public-command-failure-1",
            r#"{"program":"/bin/sh","args":["-c","printf failure-output; printf started > command-started; exit 7"]}"#,
        ),
        "mock-public-background" => (
            "run_command",
            "public-background-1",
            r#"{"program":"/bin/sh","args":["-c","sleep 30 >/dev/null 2>&1 & echo $! > background.pid"]}"#,
        ),
        "mock-public-command-timeout" => (
            "run_command",
            "public-command-timeout-1",
            r#"{"program":"/bin/sh","args":["-c","sleep 30 >/dev/null 2>&1 & echo $! > timeout-child.pid; wait"],"timeout_ms":200}"#,
        ),
        "mock-public-command-long" => (
            "run_command",
            "public-command-long-1",
            r#"{"program":"/bin/sh","args":["-c","sleep 30 >/dev/null 2>&1 & echo $! > long-child.pid; wait"]}"#,
        ),
        "mock-public-no-ctty" => (
            "run_command",
            "public-no-ctty-1",
            r#"{"program":"/usr/bin/python3","args":["-c","import os\ntry:\n    os.open('/dev/tty', os.O_RDWR)\nexcept OSError:\n    print(f'UID={os.getuid()} CWD={os.getcwd()} NO_CTTY')\nelse:\n    print('CTTY_AVAILABLE')"]}"#,
        ),
        "mock-public-ask" => {
            return Some(Box::new(MockProvider::with_tool_call(
                "ask_user_question",
                "public-ask-1",
                "{}",
            )))
        }
        "mock-public-in-band-ask" => {
            return Some(Box::new(MockProvider::text_only(
                "COSH_QUESTION:{\"question\":\"Which branch?\",\"options\":[{\"label\":\"main\"}]}",
            )))
        }
        "mock-public-loop" => {
            return Some(Box::new(MockProvider::new(
                (0..50)
                    .map(|_| {
                        vec![
                            GenerateEvent::ToolCallStart {
                                index: 0,
                                id: "public-loop-1".to_string(),
                                name: "read_file".to_string(),
                            },
                            GenerateEvent::ToolCallDelta {
                                index: 0,
                                arguments_delta: r#"{"path":"input.txt"}"#.to_string(),
                            },
                            GenerateEvent::ToolCallEnd { index: 0 },
                            GenerateEvent::MessageEnd,
                        ]
                    })
                    .collect(),
            )))
        }
        _ => return None,
    };
    Some(Box::new(MockProvider::tool_call_with_result_echo(
        tool, id, arguments,
    )))
}
