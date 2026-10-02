//! agent-sec daemon integration.

pub mod client;

pub use client::{AgentSecClient, AgentSecClientError, DaemonErrorPayload, DaemonResponse};

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{AgentSecClient, DaemonResponse};

    #[test]
    fn module_reexports_client_types() {
        let socket_path = PathBuf::from("agent-sec-daemon.sock");
        let client = AgentSecClient::new(Some(socket_path.clone()))
            .expect("explicit socket path should create a client");
        let response = DaemonResponse {
            request_id: "req-1".to_string(),
            ok: true,
            data: serde_json::Value::Null,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            error: None,
        };

        assert_eq!(client.socket_path(), &socket_path);
        assert!(response.ok);
    }
}
