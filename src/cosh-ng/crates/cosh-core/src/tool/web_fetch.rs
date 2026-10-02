//! Bounded HTTP(S) content retrieval.

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::redirect::Policy;
use serde_json::Value;

use super::{Tool, ToolContext, ToolKind, ToolResult};

const MAX_RESPONSE_BYTES: usize = 512 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct WebFetchTool {
    client: Option<reqwest::Client>,
}

impl WebFetchTool {
    pub(super) fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .redirect(Policy::limited(5))
            .build()
            .ok();
        Self { client }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch bounded text content from an HTTP or HTTPS URL. Network access requires approval outside trust mode."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Fully qualified HTTP or HTTPS URL"
                },
                "prompt": {
                    "type": "string",
                    "description": "Optional description of the information to extract"
                }
            },
            "required": ["url"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Network
    }

    async fn invoke(&self, params: Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
        let url = params
            .get("url")
            .and_then(Value::as_str)
            .ok_or("missing 'url' parameter")?;
        let parsed =
            reqwest::Url::parse(url).map_err(|error| format!("invalid URL '{url}': {error}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Ok(ToolResult::error(
                "web_fetch only supports HTTP and HTTPS URLs",
            ));
        }

        let client = self
            .client
            .as_ref()
            .ok_or("Failed to initialize the HTTP client")?;
        let response = client
            .get(parsed)
            .send()
            .await
            .map_err(|error| format!("Failed to fetch {url}: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Ok(ToolResult::error(format!(
                "Request to {url} failed with HTTP {status}"
            )));
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        if let Some(content_type) = content_type.as_deref() {
            if !is_textual_content_type(content_type) {
                return Ok(ToolResult::error(format!(
                    "Unsupported Content-Type '{content_type}' from {url}; \
                     web_fetch only supports textual responses"
                )));
            }
        }
        let content_type = content_type.unwrap_or_else(|| "unknown".to_string());
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        let mut truncated = false;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| format!("Failed to read {url}: {error}"))?;
            let remaining = MAX_RESPONSE_BYTES.saturating_sub(body.len());
            if chunk.len() > remaining {
                body.extend_from_slice(&chunk[..remaining]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
            if body.len() == MAX_RESPONSE_BYTES {
                truncated = true;
                break;
            }
        }

        let mut output = format!(
            "Source: {url}\nContent-Type: {content_type}\n\n{}",
            String::from_utf8_lossy(&body)
        );
        if truncated {
            output.push_str("\n\n[response truncated at 524288 bytes]");
        }
        Ok(ToolResult::success(output))
    }
}

fn is_textual_content_type(content_type: &str) -> bool {
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let Some((category, subtype)) = media_type.split_once('/') else {
        return false;
    };

    category == "text"
        || (category == "application"
            && (matches!(
                subtype,
                "json"
                    | "xml"
                    | "javascript"
                    | "x-javascript"
                    | "yaml"
                    | "x-yaml"
                    | "toml"
                    | "x-www-form-urlencoded"
            ) || subtype.ends_with("+json")
                || subtype.ends_with("+xml")))
        || media_type == "image/svg+xml"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn fetches_http_content() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello",
                )
                .await
                .unwrap();
        });
        let ctx = ToolContext::new(
            PathBuf::from("/tmp"),
            "test".to_string(),
            PathBuf::from("/tmp"),
        );

        let result = WebFetchTool::new()
            .invoke(
                serde_json::json!({"url": format!("http://{address}/")}),
                &ctx,
            )
            .await
            .unwrap();
        server.await.unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("Content-Type: text/plain"));
        assert!(result.output.ends_with("hello"));
    }

    #[tokio::test]
    async fn rejects_non_http_urls() {
        let ctx = ToolContext::new(
            PathBuf::from("/tmp"),
            "test".to_string(),
            PathBuf::from("/tmp"),
        );

        let result = WebFetchTool::new()
            .invoke(serde_json::json!({"url": "file:///etc/passwd"}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error);
    }

    #[tokio::test]
    async fn rejects_binary_content_types() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\n\
                      Content-Length: 4\r\n\r\n\x00\xff\x00\xff",
                )
                .await
                .unwrap();
        });
        let ctx = ToolContext::new(
            PathBuf::from("/tmp"),
            "test".to_string(),
            PathBuf::from("/tmp"),
        );

        let result = WebFetchTool::new()
            .invoke(
                serde_json::json!({"url": format!("http://{address}/document.pdf")}),
                &ctx,
            )
            .await
            .unwrap();
        server.await.unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("application/pdf"));
        assert!(result.output.contains("only supports textual responses"));
        assert!(!result.output.contains('\u{fffd}'));
    }

    #[test]
    fn recognizes_structured_text_content_types() {
        for content_type in [
            "text/plain; charset=utf-8",
            "application/json",
            "application/problem+json",
            "application/xml",
            "application/atom+xml",
            "image/svg+xml",
        ] {
            assert!(
                is_textual_content_type(content_type),
                "expected textual Content-Type: {content_type}"
            );
        }
        for content_type in ["application/pdf", "application/zip", "image/png"] {
            assert!(
                !is_textual_content_type(content_type),
                "expected binary Content-Type: {content_type}"
            );
        }
    }
}
