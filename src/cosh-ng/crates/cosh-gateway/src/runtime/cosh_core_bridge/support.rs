fn prompt_text(
    input: Vec<ContentPart>,
    maximum_bytes: usize,
) -> Result<String, AgentRuntimePortError> {
    let mut parts = Vec::with_capacity(input.len());
    let mut total_bytes = 0usize;
    for part in input {
        match part {
            ContentPart::Text { text } => {
                total_bytes = total_bytes
                    .checked_add(text.as_str().len())
                    .and_then(|total| total.checked_add(usize::from(!parts.is_empty())))
                    .ok_or(AgentRuntimePortError::Protocol)?;
                if total_bytes > maximum_bytes {
                    return Err(AgentRuntimePortError::Protocol);
                }
                parts.push(text.as_str().to_owned());
            }
            ContentPart::ResourceLink { .. } => {
                return Err(AgentRuntimePortError::Unsupported {
                    operation: "resource prompt",
                });
            }
        }
    }
    if parts.is_empty() {
        return Err(AgentRuntimePortError::Protocol);
    }
    Ok(parts.join("\n"))
}

fn safe_error(
    code: &'static str,
    category: ErrorCategory,
    retryable: bool,
    message: &'static str,
) -> ContractError {
    // Static values are kept within contract bounds and stable code syntax.
    ContractError::new(code, category, retryable, message)
        .unwrap_or_else(|_| unreachable!("static contract error must remain valid"))
}

fn digest_json(value: &impl serde::Serialize) -> Result<Digest, AgentRuntimePortError> {
    let bytes = serde_json::to_vec(value).map_err(|_| AgentRuntimePortError::Protocol)?;
    Ok(sha256_digest(&bytes))
}

fn sha256_digest(bytes: &[u8]) -> Digest {
    let digest = Sha256::digest(bytes);
    Digest::parse(format!("{digest:x}"))
        .unwrap_or_else(|_| unreachable!("SHA-256 lower hex is a valid contract digest"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
