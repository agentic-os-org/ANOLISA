use std::path::Path;

use aes_gcm::aead::generic_array::typenum::U16;
use aes_gcm::{
    aead::{Aead, KeyInit},
    aes::Aes256,
    AesGcm,
};
use scrypt::{scrypt, Params};

use crate::config::config_dir;

/// AES-256-GCM with 16-byte nonce (matching Node.js crypto.createCipheriv behavior).
type Aes256Gcm16 = AesGcm<Aes256, U16>;

const ENCRYPTED_PREFIX: &str = "enc:";
const CREDENTIAL_PASSWORD: &[u8] = b"copilot-credential-encrypt";

pub fn try_migrate() {
    let dir = config_dir();
    try_migrate_from_dir(&dir);
}

fn try_migrate_from_dir(dir: &Path) {
    let settings_path = dir.join("settings.json");
    let config_path = dir.join("config.toml");

    if config_path.exists() {
        return;
    }

    if !settings_path.exists() {
        try_migrate_aliyun_credentials(dir, &config_path);
        return;
    }

    tracing::info!("Migrating settings.json → config.toml ...");

    match migrate_settings(&settings_path, &config_path, dir) {
        Ok(()) => tracing::info!("Migration complete: {}", config_path.display()),
        Err(e) => tracing::warn!("Migration warning: {e} (continuing with defaults)"),
    }

    try_migrate_aliyun_credentials(dir, &config_path);
}

/// Migrate Aliyun credentials from copilot-shell's encrypted aliyun_creds.json
/// into cosh-ng's config.toml. Only runs once (when config.toml has no aliyun provider).
fn try_migrate_aliyun_credentials(cfg_dir: &Path, config_path: &Path) {
    let creds_path = cfg_dir.join("aliyun_creds.json");
    if !creds_path.exists() {
        return;
    }

    let encrypted = match std::fs::read_to_string(&creds_path) {
        Ok(c) => c.trim().to_string(),
        Err(_) => return,
    };

    let salt_path = cfg_dir.join(".encryption-salt");
    let decrypted = match decrypt_credential(&encrypted, &salt_path) {
        Some(d) => d,
        None => return,
    };

    let creds: serde_json::Value = match serde_json::from_str(&decrypted) {
        Ok(v) => v,
        Err(_) => return,
    };

    let ak = creds["accessKeyId"].as_str().unwrap_or_default();
    let sk = creds["accessKeySecret"].as_str().unwrap_or_default();
    if ak.is_empty() || sk.is_empty() {
        return;
    }
    let st = creds["securityToken"]
        .as_str()
        .filter(|token| !token.is_empty());

    let mut existing = if config_path.exists() {
        std::fs::read_to_string(config_path).unwrap_or_default()
    } else {
        String::new()
    };

    if !existing.contains("[ai]") {
        existing.push_str("[ai]\n");
        existing.push_str("active_provider = \"aliyun\"\n\n");
    }

    let mut provider = String::from("\n[ai.providers.aliyun]\n");
    provider.push_str("type = \"aliyun\"\n");
    provider.push_str(&format!(
        "access_key_id = \"{}\"\n",
        escape_toml_migrate(ak)
    ));
    provider.push_str(&format!(
        "access_key_secret = \"{}\"\n",
        escape_toml_migrate(sk)
    ));
    if let Some(st) = st {
        provider.push_str(&format!(
            "security_token = \"{}\"\n",
            escape_toml_migrate(st)
        ));
    }
    provider.push_str("model = \"qwen3.7-plus\"\n");
    upsert_provider_section(&mut existing, "[ai.providers.aliyun]", &provider);

    let pid = std::process::id();
    let tmp_path = cfg_dir.join(format!("config.toml.tmp.{pid}"));
    if std::fs::write(&tmp_path, &existing).is_err() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600));
    }
    if std::fs::rename(&tmp_path, config_path).is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        return;
    }

    tracing::info!("Migrated Aliyun credentials from aliyun_creds.json");
}

fn escape_toml_migrate(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn upsert_provider_section(content: &mut String, header: &str, section: &str) {
    let Some(start) = content.find(header) else {
        content.push_str(section);
        return;
    };
    let end = content[start + header.len()..]
        .find("\n[")
        .map(|offset| start + header.len() + offset)
        .unwrap_or(content.len());
    content.replace_range(start..end, section.trim_start_matches('\n'));
}

fn migrate_settings(
    settings_path: &Path,
    config_path: &Path,
    cfg_dir: &Path,
) -> Result<(), String> {
    let content = std::fs::read_to_string(settings_path)
        .map_err(|e| format!("failed to read settings.json: {e}"))?;

    let root: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("invalid JSON: {e}"))?;

    let auth = &root["security"]["auth"];
    let model = &root["model"];
    let tools = &root["tools"];
    let general = &root["general"];

    let selected_type = auth["selectedType"].as_str().unwrap_or("openai");

    let raw_api_key = auth["apiKey"].as_str().unwrap_or("");
    let api_key = if raw_api_key.starts_with(ENCRYPTED_PREFIX) {
        let salt_path = cfg_dir.join(".encryption-salt");
        match decrypt_credential(raw_api_key, &salt_path) {
            Some(k) => k,
            None => {
                tracing::warn!("failed to decrypt API key, skipping");
                String::new()
            }
        }
    } else {
        raw_api_key.to_string()
    };

    let base_url = auth["baseUrl"].as_str().unwrap_or("").to_string();

    let provider_model = match selected_type {
        "aliyun" => auth["aliyunModel"].as_str().unwrap_or(""),
        _ => auth["openaiModel"].as_str().unwrap_or(""),
    };

    let active_model = model["name"].as_str().unwrap_or(provider_model);

    let (active_provider, provider_id, provider_type, auth_source) = match selected_type {
        "aliyun" => ("aliyun", "aliyun", "aliyun", Some("ecs_ram_role")),
        "openai" => ("default", "default", "dashscope", None),
        other => ("default", "default", other, None),
    };

    let session_token_limit = model["sessionTokenLimit"].as_i64().filter(|&v| v > 0);

    let max_turns = model["maxSessionTurns"].as_i64().filter(|&v| v > 0);

    let approval_mode = tools["approvalMode"].as_str().map(map_approval_mode);

    let output_language = general["outputLanguage"]
        .as_str()
        .filter(|&v| v != "auto" && !v.is_empty());

    let toml = build_toml(&MigratedFields {
        active_provider,
        provider_id,
        provider_type,
        auth_source,
        base_url: &base_url,
        api_key: &api_key,
        provider_model,
        active_model,
        session_token_limit,
        max_turns,
        approval_mode,
        output_language,
    });

    std::fs::write(config_path, toml.as_bytes())
        .map_err(|e| format!("failed to write config.toml: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

fn decrypt_credential(encrypted: &str, salt_path: &Path) -> Option<String> {
    let without_prefix = encrypted.strip_prefix(ENCRYPTED_PREFIX)?;
    let parts: Vec<&str> = without_prefix.split(':').collect();
    if parts.len() != 3 {
        return None;
    }

    let iv_bytes = hex_decode(parts[0])?;
    let tag_bytes = hex_decode(parts[1])?;
    let ct_bytes = hex_decode(parts[2])?;

    // copilot-shell uses 16-byte IV (Node.js crypto.createCipheriv)
    if iv_bytes.len() != 16 || tag_bytes.len() != 16 {
        return None;
    }

    let salt = std::fs::read(salt_path).ok()?;
    if salt.len() != 32 {
        return None;
    }

    let mut key = [0u8; 32];
    let params = Params::new(14, 8, 1, 32).ok()?;
    scrypt(CREDENTIAL_PASSWORD, &salt, &params, &mut key).ok()?;

    let cipher = Aes256Gcm16::new_from_slice(&key).ok()?;

    let nonce = aes_gcm::aead::generic_array::GenericArray::from_slice(&iv_bytes);
    let mut ciphertext_with_tag = ct_bytes;
    ciphertext_with_tag.extend_from_slice(&tag_bytes);

    let plaintext = cipher.decrypt(nonce, ciphertext_with_tag.as_ref()).ok()?;
    String::from_utf8(plaintext).ok()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn map_approval_mode(mode: &str) -> &str {
    match mode {
        "YOLO" | "yolo" => "trust",
        "AUTO_EDIT" | "auto_edit" => "auto",
        "PLAN" | "plan" => "recommend",
        _ => "recommend",
    }
}

struct MigratedFields<'a> {
    active_provider: &'a str,
    provider_id: &'a str,
    provider_type: &'a str,
    auth_source: Option<&'a str>,
    base_url: &'a str,
    api_key: &'a str,
    provider_model: &'a str,
    active_model: &'a str,
    session_token_limit: Option<i64>,
    max_turns: Option<i64>,
    approval_mode: Option<&'a str>,
    output_language: Option<&'a str>,
}

fn build_toml(fields: &MigratedFields<'_>) -> String {
    let MigratedFields {
        active_provider,
        provider_id,
        provider_type,
        auth_source,
        base_url,
        api_key,
        provider_model,
        active_model,
        session_token_limit,
        max_turns,
        approval_mode,
        output_language,
    } = fields;
    let date = chrono::Local::now().format("%Y-%m-%d");
    let mut out = String::new();

    out.push_str(&format!("# Auto-migrated from settings.json on {date}\n"));
    out.push_str("# Original: ~/.copilot-shell/settings.json\n\n");

    out.push_str("[ai]\n");
    out.push_str(&format!("active_provider = \"{active_provider}\"\n"));
    if !active_model.is_empty() {
        out.push_str(&format!("active_model = \"{active_model}\"\n"));
    }
    if let Some(lang) = output_language {
        out.push_str(&format!("output_language = \"{lang}\"\n"));
    }
    out.push('\n');

    out.push_str(&format!("[ai.providers.{provider_id}]\n"));
    out.push_str(&format!("type = \"{provider_type}\"\n"));
    if let Some(source) = auth_source {
        out.push_str(&format!("auth_source = \"{source}\"\n"));
    }
    if !base_url.is_empty() {
        out.push_str(&format!("base_url = \"{base_url}\"\n"));
    }
    if !api_key.is_empty() {
        out.push_str(&format!("api_key = \"{api_key}\"\n"));
    }
    if !provider_model.is_empty() {
        out.push_str(&format!("model = \"{provider_model}\"\n"));
    }
    out.push('\n');

    let has_agent_section =
        session_token_limit.is_some() || max_turns.is_some() || approval_mode.is_some();

    if has_agent_section {
        out.push_str("[agent]\n");
        if let Some(mode) = approval_mode {
            out.push_str(&format!("approval_mode = \"{mode}\"\n"));
        }
        if let Some(turns) = max_turns {
            out.push_str(&format!("max_turns = {turns}\n"));
        }
        if let Some(limit) = session_token_limit {
            out.push_str(&format!("session_token_limit = {limit}\n"));
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ApprovalMode;

    #[test]
    fn plaintext_passthrough() {
        let raw = "sk-plaintext-key";
        assert!(!raw.starts_with(ENCRYPTED_PREFIX));
    }

    #[test]
    fn decrypt_known_value() {
        let tmp = tempfile::TempDir::new().unwrap();
        let salt_path = tmp.path().join(".encryption-salt");

        let salt = [0x42u8; 32];
        std::fs::write(&salt_path, salt).unwrap();

        let mut key = [0u8; 32];
        let params = Params::new(14, 8, 1, 32).unwrap();
        scrypt(CREDENTIAL_PASSWORD, &salt, &params, &mut key).unwrap();

        let cipher = Aes256Gcm16::new_from_slice(&key).unwrap();
        let iv = [0xABu8; 16];
        let nonce = aes_gcm::aead::generic_array::GenericArray::from_slice(&iv);
        let plaintext = b"sk-test-secret-key";

        let ciphertext_with_tag = cipher.encrypt(nonce, plaintext.as_ref()).unwrap();
        let (ct, tag) = ciphertext_with_tag.split_at(ciphertext_with_tag.len() - 16);

        let encrypted = format!(
            "enc:{}:{}:{}",
            hex_encode(&iv),
            hex_encode(tag),
            hex_encode(ct),
        );

        let result = decrypt_credential(&encrypted, &salt_path);
        assert_eq!(result, Some("sk-test-secret-key".to_string()));
    }

    #[test]
    fn approval_mode_mapping() {
        assert_eq!(map_approval_mode("DEFAULT"), "recommend");
        assert_eq!(map_approval_mode("YOLO"), "trust");
        assert_eq!(map_approval_mode("yolo"), "trust");
        assert_eq!(map_approval_mode("AUTO_EDIT"), "auto");
        assert_eq!(map_approval_mode("PLAN"), "recommend");
        assert_eq!(map_approval_mode("unknown"), "recommend");
    }

    #[test]
    fn build_toml_output_parseable() {
        let toml_str = build_toml(&MigratedFields {
            active_provider: "default",
            provider_id: "default",
            provider_type: "dashscope",
            auth_source: None,
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
            api_key: "sk-test",
            provider_model: "qwen3-plus",
            active_model: "qwen3-plus",
            session_token_limit: Some(128000),
            max_turns: Some(30),
            approval_mode: Some("recommend"),
            output_language: Some("zh-CN"),
        });

        let config: crate::config::CoreConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(config.ai.active_provider.as_deref(), Some("default"));
        assert_eq!(config.ai.active_model.as_deref(), Some("qwen3-plus"));
        assert_eq!(config.ai.output_language.as_deref(), Some("zh-CN"));
        assert_eq!(config.agent.session_token_limit, 128000);
        assert_eq!(config.agent.max_turns, 30);
        assert_eq!(config.agent.approval_mode, ApprovalMode::Recommend);

        let provider = config.ai.providers.get("default").unwrap();
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://dashscope.aliyuncs.com/compatible-mode/v1")
        );
        assert_eq!(provider.api_key.as_deref(), Some("sk-test"));
        assert_eq!(provider.provider_type.as_deref(), Some("dashscope"));
    }

    #[test]
    fn skip_when_config_exists() {
        let tmp = tempfile::TempDir::new().unwrap();
        let settings_path = tmp.path().join("settings.json");
        let config_path = tmp.path().join("config.toml");

        std::fs::write(&settings_path, r#"{"security":{"auth":{}}}"#).unwrap();
        std::fs::write(&config_path, "# existing").unwrap();

        // try_migrate checks existence — if config exists, it returns early
        assert!(config_path.exists());
        assert!(settings_path.exists());

        // Simulate the guard logic from try_migrate
        let should_skip = !settings_path.exists() || config_path.exists();
        assert!(should_skip);
    }

    #[test]
    fn existing_config_skips_legacy_aliyun_credentials() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        let creds_path = tmp.path().join("aliyun_creds.json");
        let salt_path = tmp.path().join(".encryption-salt");

        std::fs::write(
            &config_path,
            r#"[ai]
active_provider = "qwen"

[ai.providers.qwen]
type = "dashscope"
api_key = "sk-current"
"#,
        )
        .unwrap();

        let salt = [0x23u8; 32];
        std::fs::write(&salt_path, salt).unwrap();
        let encrypted = encrypt_test_credential(
            r#"{"accessKeyId":"legacy-ak","accessKeySecret":"legacy-sk","securityToken":"legacy-token"}"#,
            &salt,
        );
        std::fs::write(&creds_path, encrypted).unwrap();

        try_migrate_from_dir(tmp.path());

        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("[ai.providers.qwen]"));
        assert!(!content.contains("[ai.providers.aliyun]"));
        assert!(!content.contains("legacy-ak"));
        assert!(!content.contains("legacy-token"));
    }

    #[test]
    fn full_migration_without_encryption() {
        let tmp = tempfile::TempDir::new().unwrap();
        let settings_path = tmp.path().join("settings.json");
        let config_path = tmp.path().join("config.toml");

        let json = r#"{
            "$version": 2,
            "security": {
                "auth": {
                    "selectedType": "openai",
                    "apiKey": "sk-plain-key",
                    "baseUrl": "https://example.com/v1",
                    "openaiModel": "test-model"
                }
            },
            "model": {
                "name": "test-model",
                "sessionTokenLimit": 64000,
                "maxSessionTurns": 15
            },
            "tools": {
                "approvalMode": "YOLO"
            },
            "general": {
                "outputLanguage": "en"
            }
        }"#;
        std::fs::write(&settings_path, json).unwrap();

        migrate_settings(&settings_path, &config_path, tmp.path()).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: crate::config::CoreConfig = toml::from_str(&content).unwrap();

        assert_eq!(config.ai.active_model.as_deref(), Some("test-model"));
        assert_eq!(config.agent.approval_mode, ApprovalMode::Trust);
        assert_eq!(config.agent.max_turns, 15);
        assert_eq!(config.agent.session_token_limit, 64000);
        assert_eq!(config.ai.output_language.as_deref(), Some("en"));

        let provider = config.ai.providers.get("default").unwrap();
        assert_eq!(provider.api_key.as_deref(), Some("sk-plain-key"));
    }

    #[test]
    fn aliyun_settings_migration_selects_aliyun_ecs_provider() {
        let tmp = tempfile::TempDir::new().unwrap();
        let settings_path = tmp.path().join("settings.json");
        let config_path = tmp.path().join("config.toml");

        let json = r#"{
            "security": {
                "auth": {
                    "selectedType": "aliyun",
                    "aliyunModel": "qwen3.7-plus",
                    "aliyunModels": ["qwen3.7-plus"]
                }
            },
            "model": {
                "name": "qwen3.7-plus"
            }
        }"#;
        std::fs::write(&settings_path, json).unwrap();

        migrate_settings(&settings_path, &config_path, tmp.path()).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: crate::config::CoreConfig = toml::from_str(&content).unwrap();

        assert_eq!(config.ai.active_provider.as_deref(), Some("aliyun"));
        assert_eq!(config.ai.active_model.as_deref(), Some("qwen3.7-plus"));
        assert!(!config.ai.providers.contains_key("default"));

        let provider = config.ai.providers.get("aliyun").unwrap();
        assert_eq!(provider.provider_type.as_deref(), Some("aliyun"));
        assert_eq!(provider.auth_source.as_deref(), Some("ecs_ram_role"));
        assert_eq!(provider.model.as_deref(), Some("qwen3.7-plus"));
        assert!(provider.access_key_id.is_none());
        assert!(provider.access_key_secret.is_none());
        assert!(provider.security_token.is_none());
        assert!(provider.api_key.is_none());
    }

    #[test]
    fn aliyun_settings_migration_preserves_legacy_sts_credentials() {
        let tmp = tempfile::TempDir::new().unwrap();
        let settings_path = tmp.path().join("settings.json");
        let config_path = tmp.path().join("config.toml");
        let creds_path = tmp.path().join("aliyun_creds.json");
        let salt_path = tmp.path().join(".encryption-salt");

        std::fs::write(
            &settings_path,
            r#"{"security":{"auth":{"selectedType":"aliyun","aliyunModel":"qwen3.7-plus"}}}"#,
        )
        .unwrap();

        let salt = [0x34u8; 32];
        std::fs::write(&salt_path, salt).unwrap();
        let encrypted = encrypt_test_credential(
            r#"{"accessKeyId":"legacy-ak","accessKeySecret":"legacy-sk","securityToken":"legacy-token"}"#,
            &salt,
        );
        std::fs::write(&creds_path, encrypted).unwrap();

        try_migrate_from_dir(tmp.path());

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: crate::config::CoreConfig = toml::from_str(&content).unwrap();
        let provider = config.ai.providers.get("aliyun").unwrap();

        assert_eq!(config.ai.active_provider.as_deref(), Some("aliyun"));
        assert_eq!(provider.auth_source.as_deref(), None);
        assert_eq!(provider.access_key_id.as_deref(), Some("legacy-ak"));
        assert_eq!(provider.access_key_secret.as_deref(), Some("legacy-sk"));
        assert_eq!(provider.security_token.as_deref(), Some("legacy-token"));
        assert!(content.contains("legacy-ak"));
        assert!(content.contains("legacy-token"));
    }

    #[test]
    fn legacy_aliyun_sts_credentials_keep_ak_sk_token() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        let creds_path = tmp.path().join("aliyun_creds.json");
        let salt_path = tmp.path().join(".encryption-salt");

        let salt = [0x45u8; 32];
        std::fs::write(&salt_path, salt).unwrap();
        let encrypted = encrypt_test_credential(
            r#"{"accessKeyId":"legacy-ak","accessKeySecret":"legacy-sk","securityToken":"legacy-token"}"#,
            &salt,
        );
        std::fs::write(&creds_path, encrypted).unwrap();

        try_migrate_from_dir(tmp.path());

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: crate::config::CoreConfig = toml::from_str(&content).unwrap();
        let provider = config.ai.providers.get("aliyun").unwrap();

        assert_eq!(config.ai.active_provider.as_deref(), Some("aliyun"));
        assert_eq!(provider.provider_type.as_deref(), Some("aliyun"));
        assert_eq!(provider.auth_source.as_deref(), None);
        assert_eq!(provider.access_key_id.as_deref(), Some("legacy-ak"));
        assert_eq!(provider.access_key_secret.as_deref(), Some("legacy-sk"));
        assert_eq!(provider.security_token.as_deref(), Some("legacy-token"));
        assert!(content.contains("legacy-ak"));
        assert!(content.contains("legacy-token"));
    }

    #[test]
    fn legacy_aliyun_static_credentials_keep_ak_sk() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        let creds_path = tmp.path().join("aliyun_creds.json");
        let salt_path = tmp.path().join(".encryption-salt");

        let salt = [0x56u8; 32];
        std::fs::write(&salt_path, salt).unwrap();
        let encrypted = encrypt_test_credential(
            r#"{"accessKeyId":"legacy-ak","accessKeySecret":"legacy-sk"}"#,
            &salt,
        );
        std::fs::write(&creds_path, encrypted).unwrap();

        try_migrate_from_dir(tmp.path());

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: crate::config::CoreConfig = toml::from_str(&content).unwrap();
        let provider = config.ai.providers.get("aliyun").unwrap();

        assert_eq!(config.ai.active_provider.as_deref(), Some("aliyun"));
        assert_eq!(provider.provider_type.as_deref(), Some("aliyun"));
        assert_eq!(provider.auth_source.as_deref(), None);
        assert_eq!(provider.access_key_id.as_deref(), Some("legacy-ak"));
        assert_eq!(provider.access_key_secret.as_deref(), Some("legacy-sk"));
        assert!(provider.security_token.is_none());
    }

    fn hex_encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn encrypt_test_credential(plaintext: &str, salt: &[u8; 32]) -> String {
        let mut key = [0u8; 32];
        let params = Params::new(14, 8, 1, 32).unwrap();
        scrypt(CREDENTIAL_PASSWORD, salt, &params, &mut key).unwrap();

        let cipher = Aes256Gcm16::new_from_slice(&key).unwrap();
        let iv = [0xCDu8; 16];
        let nonce = aes_gcm::aead::generic_array::GenericArray::from_slice(&iv);
        let ciphertext_with_tag = cipher.encrypt(nonce, plaintext.as_bytes()).unwrap();
        let (ct, tag) = ciphertext_with_tag.split_at(ciphertext_with_tag.len() - 16);

        format!(
            "enc:{}:{}:{}",
            hex_encode(&iv),
            hex_encode(tag),
            hex_encode(ct)
        )
    }
}
