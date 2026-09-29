use super::*;

/// Fake cosh-core exposing a single OpenAI Compatible template and logging registry traffic.
///
/// Slash auth asks for `provider_id` only when the same template is already configured.
const AUTH_REGISTRY_CORE: &str = r#"#!/bin/sh
if [ "$1" = "--registry" ]; then
  read -r request
  printf '%s\n' "$request" >> "$AUTH_REGISTRY_LOG"
  case "$request" in
    *'"action":"state"'*)
      printf '%s%s%s\n' '{"type":"registry_response","request_id":"reg","success":true,"data":{"templates":[{"id":"openai_compat","label":"OpenAI Compatible","fields":[{"name":"base_url","label":"Base URL","hint":null,"secret":false,"required":true,"placeholder":null},{"name":"api_key","label":"API Key","hint":null,"secret":true,"required":true,"placeholder":null},{"name":"model","label":"Model","hint":null,"secret":false,"required":true,"placeholder":null}]}],"saved_providers":' "${AUTH_SAVED_PROVIDERS:-[]}" '}}'
      ;;
    *'"action":"configure"'*)
      if [ -n "$AUTH_CONFIGURE_ERROR" ]; then
        printf '%s\n' '{"type":"registry_response","request_id":"reg","success":false,"data":{"error_code":"invalid_credentials"},"error":"The API key was rejected. Check the API Key and try again."}'
      else
        printf '%s\n' '{"type":"registry_response","request_id":"reg","success":true,"data":{"configured":true}}'
      fi
      ;;
    *)
      printf '%s\n' '{"type":"registry_response","request_id":"reg","success":true,"data":{"model":"main-model","configured":true}}'
      ;;
  esac
  exit 0
fi
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"auth-inline","model":"main-model","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"auth-inline","is_error":false,"result":"done"}'
"#;

#[test]
fn raw_cli_auth_failure_keeps_panel_and_never_claims_success() {
    let home = temp_shell_home("auth-preflight-failure");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(&cosh_core_path, AUTH_REGISTRY_CORE);
    let registry_log = home.join("registry.log");

    let home_str = home.to_string_lossy().to_string();
    let core_str = cosh_core_path.to_string_lossy().to_string();
    let log_str = registry_log.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "cosh-core",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", &core_str),
            ("AUTH_REGISTRY_LOG", &log_str),
            ("AUTH_CONFIGURE_ERROR", "1"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            ("Select your AI provider:", b"\n".as_slice()),
            ("Enter Base URL", b"http://127.0.0.1:1/v1\n".as_slice()),
            ("Enter API Key", b"sk-rejected\n".as_slice()),
            ("Enter Model", b"test-model\n".as_slice()),
            ("Credentials were not saved", b"".as_slice()),
        ],
    );
    let requests = fs::read_to_string(&registry_log).unwrap_or_default();
    let _ = fs::remove_dir_all(&home);
    let compact = compact_terminal_words(&output);

    assert!(compact.contains("Validating configuration..."), "{output}");
    assert!(
        compact.contains("Checking endpoint, credentials, and model."),
        "{output}"
    );
    assert!(compact.contains("Credentials were not saved"), "{output}");
    let failure = compact
        .rfind("Credentials were not saved")
        .expect("failure panel is present");
    assert!(compact[failure..].contains("Enter API Key"), "{output}");
    assert!(!compact.contains("Auth configured"), "{output}");
    assert!(!compact.contains("credentials saved"), "{output}");
    assert!(!compact.contains("Provider ID"), "{output}");
    assert_eq!(action_count(&requests, "configure"), 1, "{requests}");
    let configure = requests
        .lines()
        .find(|line| line.contains(r#""action":"configure""#))
        .unwrap_or_else(|| panic!("expected configure request: {requests}"));
    assert!(
        configure.contains(r#""provider_id":"openai_compat""#),
        "{configure}"
    );
}

/// A dotted Provider ID must be rejected on the spot instead of at the final `configure`.
#[test]
fn raw_cli_auth_dotted_provider_id_can_be_corrected() {
    let home = temp_shell_home("auth-dotted-provider-id");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(&cosh_core_path, AUTH_REGISTRY_CORE);
    let registry_log = home.join("registry.log");

    let home_str = home.to_string_lossy().to_string();
    let core_str = cosh_core_path.to_string_lossy().to_string();
    let log_str = registry_log.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "cosh-core",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", &core_str),
            ("AUTH_REGISTRY_LOG", &log_str),
            ("AUTH_SAVED_PROVIDERS", SAVED_OPENAI_COMPAT),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            ("+ Add new provider", b"\x1b[C\n".as_slice()),
            ("Authentication Required", b"\n".as_slice()),
            ("Type answer | Enter send", b"qwen3.7-max\n".as_slice()),
            ("Provider ID allows letters", b"\x7f".as_slice()),
            ("> qwen3.7-ma", b"\x7f".as_slice()),
            ("> qwen3.7-m", b"\x7f".as_slice()),
            ("> qwen3.7-", b"\x7f".as_slice()),
            ("> qwen3.7", b"\x7f".as_slice()),
            ("> qwen3.", b"\x7f".as_slice()),
            ("> qwen3", b"\x7f".as_slice()),
            ("> qwen", b"\x7f".as_slice()),
            ("> qwe", b"\x7f".as_slice()),
            ("> qw", b"\x7f".as_slice()),
            ("> q", b"\x7fqwen-prod\n".as_slice()),
            ("Enter Base URL", b"\x03".as_slice()),
            ("Auth cancelled", b"".as_slice()),
        ],
    );
    let requests = fs::read_to_string(&registry_log).unwrap_or_default();
    let _ = fs::remove_dir_all(&home);

    let compact = compact_terminal_words(&output);
    // The rejected field stays editable and accepts a corrected value.
    assert!(
        compact.contains("Enter Provider ID"),
        "expected Provider ID prompt: {output}"
    );
    assert!(
        compact.contains("Enter Base URL"),
        "corrected Provider ID did not advance: {output}"
    );
    assert!(
        compact.contains("Provider ID allows letters, digits, '-' and '_' only (no '.')"),
        "expected inline Provider ID error: {output}"
    );
    // The hint states the rule before the user types anything.
    assert!(
        compact.contains("Config name (not model name; letters, digits, '-' and '_' only)"),
        "expected character rule in hint: {output}"
    );
    assert!(
        compact.contains("Auth cancelled"),
        "Ctrl+C did not cancel the corrected flow: {output}"
    );
    // Nothing reached cosh-core: no configure round-trip, no late failure panel.
    assert!(
        !requests.contains(r#""action":"configure""#),
        "auth configure must not be called: {requests}"
    );
    assert!(
        !output.contains("Credentials were not saved"),
        "late core rejection panel should not appear: {output}"
    );
    assert!(
        requests.contains(r#""action":"state""#),
        "expected auth state query: {requests}"
    );
}

/// Fake cosh-core for the `/auth` management menu.
///
/// Serves the scripted `auth.state` / `auth.prepare` replies from the environment so a
/// single script covers ECS and non-ECS hosts, and appends every request to
/// `$AUTH_REGISTRY_LOG` so tests can count ECS probes.
const AUTH_MENU_CORE: &str = r#"#!/bin/sh
if [ "$1" = "--registry" ]; then
  read -r request
  printf '%s\n' "$request" >> "$AUTH_REGISTRY_LOG"
  request_id=${request#*'"request_id":"'}
  request_id=${request_id%%'"'*}
  case "$request" in
    *'"action":"state"'*)
      response=$AUTH_STATE
      ;;
    *'"action":"prepare"'*)
      response=$AUTH_PREPARE
      ;;
    *'"action":"verify"'*)
      response='{"type":"registry_response","request_id":"reg","success":true,"data":{"status":"ready"}}'
      ;;
    *)
      response='{"type":"registry_response","request_id":"reg","success":true,"data":{"model":"main-model","configured":true}}'
      ;;
  esac
  # Async probes require the caller's correlation ID, not the fixture's placeholder.
  prefix=${response%%'"request_id":"'*}
  suffix=${response#*'"request_id":"'}
  suffix=${suffix#*'"'}
  printf '%s"request_id":"%s"%s\n' "$prefix" "$request_id" "$suffix"
  exit 0
fi
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"auth-inline","model":"main-model","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"auth-inline","is_error":false,"result":"done"}'
"#;

const AUTH_TEMPLATES: &str = r#"[{"id":"aliyun","label":"Aliyun Authentication","description":"Free with limited quota","fields":[{"name":"access_key_id","label":"Access Key ID","hint":null,"secret":true,"required":true,"placeholder":null},{"name":"access_key_secret","label":"Access Key Secret","hint":null,"secret":true,"required":true,"placeholder":null},{"name":"model","label":"Model","hint":null,"secret":false,"required":false,"placeholder":"qwen3.7-plus"}]},{"id":"coding_plan","label":"Coding Plan","description":"For individual developers • Weekly quota included","builtin_base_url":"https://coding.dashscope.aliyuncs.com/v1","fields":[{"name":"api_key","label":"API Key","hint":"Plan keys start with sk-sp-.","secret":true,"required":true,"placeholder":null},{"name":"model","label":"Model","hint":null,"secret":false,"required":false,"placeholder":"qwen3.7-plus"}]},{"id":"token_plan","label":"Token Plan","description":"For teams and companies • Usage-based billing with dedicated capacity","builtin_base_url":"https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1","fields":[{"name":"api_key","label":"API Key","hint":"Plan keys start with sk-sp-.","secret":true,"required":true,"placeholder":null},{"name":"model","label":"Model","hint":null,"secret":false,"required":false,"placeholder":"qwen3.7-plus"}]},{"id":"dashscope","label":"DashScope (百炼)","description":"Connect with an existing Bailian API key","builtin_base_url":"https://dashscope.aliyuncs.com/compatible-mode/v1","fields":[{"name":"api_key","label":"API Key","hint":null,"secret":true,"required":true,"placeholder":null},{"name":"model","label":"Model","hint":null,"secret":false,"required":false,"placeholder":"qwen3.7-plus"}]},{"id":"openai_compat","label":"OpenAI Compatible","description":"Use an existing OpenAI-compatible Base URL and API key","fields":[{"name":"base_url","label":"Base URL","hint":null,"secret":false,"required":true,"placeholder":null},{"name":"api_key","label":"API Key","hint":null,"secret":true,"required":true,"placeholder":null},{"name":"model","label":"Model","hint":null,"secret":false,"required":true,"placeholder":null}]}]"#;

const SAVED_NONE: &str = "[]";

const SAVED_OPENAI_COMPAT: &str = r#"[{"provider_id":"prod","provider_type":"openai_compat","source":"user","editable":true,"auth_source":null,"model":"test-model","base_url":"https://example.invalid/v1","api_key_len":8,"active":true}]"#;

const SAVED_ALIYUN_MANUAL: &str = r#"[{"provider_id":"aliyun-manual","provider_type":"aliyun","source":"user","editable":true,"auth_source":"manual","model":"qwen3.7-plus","base_url":null,"active":true}]"#;

const SAVED_DASHSCOPE: &str = r#"[{"provider_id":"qwen-prod","provider_type":"dashscope","source":"user","editable":true,"auth_source":null,"model":"qwen3.7-plus","base_url":null,"api_key_len":8,"active":true}]"#;

const SAVED_CODING_PLAN: &str = r#"[{"provider_id":"coding-prod","provider_type":"openai","source":"user","editable":true,"auth_source":null,"model":"qwen3.7-plus","base_url":"https://coding.dashscope.aliyuncs.com/v1","api_key_len":12,"active":true}]"#;

/// A saved SysOM setup is the `aliyun` provider with `auth_source=ecs_ram_role`.
const SAVED_DASHSCOPE_AND_ECS: &str = r#"[{"provider_id":"qwen-prod","provider_type":"dashscope","source":"user","editable":true,"auth_source":null,"model":"qwen3.7-plus","base_url":null,"api_key_len":8,"active":true},{"provider_id":"sysom-trial","provider_type":"aliyun","source":"user","editable":true,"auth_source":"ecs_ram_role","model":"qwen3.7-plus","base_url":null,"active":false}]"#;

const ECS_PREPARE: &str = r#"{"type":"registry_response","request_id":"reg","success":true,"data":{"mode":"ecs_ram_role","instance_id":"i-fake-ecs-1","console_url":"https://alinux.console.aliyun.com/cn-test/guide/cosh?instance=i-fake-ecs-1","values":{"auth_source":"ecs_ram_role"}}}"#;

const MANUAL_PREPARE: &str =
    r#"{"type":"registry_response","request_id":"reg","success":true,"data":{"mode":"manual"}}"#;

const FAILING_PREPARE: &str = r#"{"type":"registry_response","request_id":"reg","success":false,"error":"ecs metadata service unavailable"}"#;

const SYSOM_ROW: &str = "SysOM (free trial, uses this ECS instance's RAM role)";

fn auth_state_response(saved_providers: &str) -> String {
    format!(
        r#"{{"type":"registry_response","request_id":"reg","success":true,"data":{{"templates":{AUTH_TEMPLATES},"saved_providers":{saved_providers}}}}}"#
    )
}

/// Drives `/auth` against the scripted core and returns `(terminal output, registry log)`.
fn run_auth_menu_flow(
    name: &str,
    saved_providers: &str,
    prepare: &str,
    steps: &[(&str, &[u8])],
) -> (String, String) {
    let home = temp_shell_home(name);
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(&cosh_core_path, AUTH_MENU_CORE);
    let registry_log = home.join("registry.log");

    let home_str = home.to_string_lossy().to_string();
    let core_str = cosh_core_path.to_string_lossy().to_string();
    let log_str = registry_log.to_string_lossy().to_string();
    let state = auth_state_response(saved_providers);
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "cosh-core",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", &core_str),
            ("AUTH_REGISTRY_LOG", &log_str),
            ("AUTH_STATE", &state),
            ("AUTH_PREPARE", prepare),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        steps,
    );
    let requests = fs::read_to_string(&registry_log).unwrap_or_default();
    let _ = fs::remove_dir_all(&home);
    (output, requests)
}

fn action_count(requests: &str, action: &str) -> usize {
    requests.matches(&format!(r#""action":"{action}""#)).count()
}

/// On ECS the free trial is the first thing `/auth` offers, even with nothing configured.
#[test]
fn raw_cli_auth_ecs_offers_sysom_first_without_saved_providers() {
    let (output, requests) = run_auth_menu_flow(
        "auth-ecs-no-saved",
        SAVED_NONE,
        ECS_PREPARE,
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            ("+ Add new provider", b"".as_slice()),
        ],
    );

    let compact = compact_terminal_words(&output);
    assert!(compact.contains("Provider Management"), "{output}");
    // Selected by default: the panel marks the current row with '>'.
    assert!(compact.contains(&format!("> [1] {SYSOM_ROW}")), "{output}");
    assert!(compact.contains("[2] + Add new provider"), "{output}");
    // The template picker must not be what an ECS user sees first.
    assert!(!compact.contains("Authentication Required"), "{output}");
    assert_eq!(action_count(&requests, "prepare"), 1, "{requests}");
}

/// The SysOM row shifts the saved providers down without breaking their action menus.
#[test]
fn raw_cli_auth_ecs_keeps_saved_provider_reachable_below_sysom() {
    let (output, requests) = run_auth_menu_flow(
        "auth-ecs-saved-dashscope",
        SAVED_DASHSCOPE,
        ECS_PREPARE,
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            ("+ Add new provider", b"\x1b[C\n".as_slice()),
            ("\"qwen-prod\":", b"".as_slice()),
        ],
    );

    let compact = compact_terminal_words(&output);
    assert!(compact.contains(&format!("> [1] {SYSOM_ROW}")), "{output}");
    assert!(compact.contains("[2] * [active] DashScope"), "{output}");
    assert!(compact.contains("[3] + Add new provider"), "{output}");
    // Row 2 opened the saved provider's action menu, not the SysOM shortcut.
    assert!(compact.contains("Edit configuration"), "{output}");
    assert!(compact.contains("Delete provider"), "{output}");
    assert!(!compact.contains("Enter Provider ID"), "{output}");
    assert_eq!(action_count(&requests, "prepare"), 1, "{requests}");
    assert_eq!(action_count(&requests, "configure"), 0, "{requests}");
}

/// An already-configured RAM-role provider is promoted instead of duplicated.
#[test]
fn raw_cli_auth_ecs_promotes_configured_ram_role_provider() {
    let (output, requests) = run_auth_menu_flow(
        "auth-ecs-saved-ram-role",
        SAVED_DASHSCOPE_AND_ECS,
        ECS_PREPARE,
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            ("+ Add new provider", b"\n".as_slice()),
            ("\"sysom-trial\":", b"".as_slice()),
        ],
    );

    let compact = compact_terminal_words(&output);
    // Row 1 is the saved provider wearing the SysOM label, and there is no extra row for it.
    assert!(
        compact.contains(&format!("> [1] {SYSOM_ROW} - \"sysom-trial\"")),
        "{output}"
    );
    assert!(!compact.contains(&format!("[2] {SYSOM_ROW}")), "{output}");
    assert!(compact.contains("[2] * [active] DashScope"), "{output}");
    assert!(compact.contains("[3] + Add new provider"), "{output}");
    // Selecting it opens the ordinary action menu instead of re-running the setup flow.
    assert!(compact.contains("Set as active provider"), "{output}");
    assert!(compact.contains("Edit configuration"), "{output}");
    assert!(compact.contains("Delete provider"), "{output}");
    assert!(!compact.contains("Enter Provider ID"), "{output}");
    assert_eq!(action_count(&requests, "prepare"), 1, "{requests}");
    assert_eq!(action_count(&requests, "delete"), 0, "{requests}");
}

/// A non-ECS host shows the plan-oriented provider menu with guidance.
#[test]
fn raw_cli_auth_non_ecs_without_saved_providers_shows_plan_entries() {
    let (output, requests) = run_auth_menu_flow(
        "auth-non-ecs-no-saved",
        SAVED_NONE,
        MANUAL_PREPARE,
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            ("Authentication Required", b"".as_slice()),
        ],
    );

    let compact = compact_terminal_words(&output);
    assert!(compact.contains("> [1] Aliyun Authentication"), "{output}");
    assert!(compact.contains("[2] Coding Plan"), "{output}");
    assert!(compact.contains("[3] Token Plan"), "{output}");
    assert!(compact.contains("[4] DashScope"), "{output}");
    assert!(compact.contains("[5] OpenAI Compatible"), "{output}");
    assert!(compact.contains("Free with limited quota"), "{output}");
    assert!(
        compact.contains("For individual developers • Weekly quota included"),
        "{output}"
    );
    assert!(
        compact.contains("For teams and companies • Usage-based billing with dedicated capacity"),
        "{output}"
    );
    assert!(!compact.contains("SysOM"), "{output}");
    assert!(!compact.contains("Provider Management"), "{output}");
    assert_eq!(action_count(&requests, "prepare"), 1, "{requests}");
}

/// A non-ECS host keeps "saved providers + Add new provider", including Edit.
#[test]
fn raw_cli_auth_non_ecs_keeps_saved_providers_then_add_new() {
    let (output, requests) = run_auth_menu_flow(
        "auth-non-ecs-saved",
        SAVED_DASHSCOPE,
        MANUAL_PREPARE,
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            ("+ Add new provider", b"\n".as_slice()),
            ("Edit configuration", b"\n".as_slice()),
            ("Edit API Key", b"".as_slice()),
        ],
    );

    let compact = compact_terminal_words(&output);
    assert!(compact.contains("> [1] * [active] DashScope"), "{output}");
    assert!(compact.contains("[2] + Add new provider"), "{output}");
    assert!(!compact.contains("[3] + Add new provider"), "{output}");
    assert!(!compact.contains("SysOM"), "{output}");
    // Edit still pre-fills the masked API key and offers to keep it.
    assert!(compact.contains("Edit API Key"), "{output}");
    assert!(compact.contains("Enter to keep current value"), "{output}");
    assert_eq!(action_count(&requests, "configure"), 0, "{requests}");
}

#[test]
fn raw_cli_auth_edits_coding_plan_with_its_original_template() {
    let (output, requests) = run_auth_menu_flow(
        "auth-edit-coding-plan",
        SAVED_CODING_PLAN,
        MANUAL_PREPARE,
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            ("coding-prod", b"\n".as_slice()),
            ("Edit configuration", b"\n".as_slice()),
            ("Edit API Key", b"".as_slice()),
        ],
    );

    let compact = compact_terminal_words(&output);
    assert!(compact.contains("Coding Plan"), "{output}");
    assert!(compact.contains("Edit API Key"), "{output}");
    assert!(
        !compact.contains("Edit Base URL"),
        "plan edit fell back to the OpenAI-compatible template: {output}"
    );
    assert_eq!(action_count(&requests, "configure"), 0, "{requests}");
}

/// The shortcut is not a blank cheque: the Provider ID rule still applies to it.
#[test]
fn raw_cli_auth_sysom_shortcut_still_validates_provider_id() {
    let (output, requests) = run_auth_menu_flow(
        "auth-sysom-bad-id",
        SAVED_ALIYUN_MANUAL,
        ECS_PREPARE,
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            ("+ Add new provider", b"\n".as_slice()),
            ("Enter Provider ID", b"bad.provider\n".as_slice()),
            ("Provider ID allows letters", b"".as_slice()),
        ],
    );

    let compact = compact_terminal_words(&output);
    assert!(
        compact.contains("Provider ID allows letters, digits, '-' and '_' only (no '.')"),
        "{output}"
    );
    // A rejected id must not reach the challenge or start verification/configuration.
    assert!(!compact.contains("ECS Instance ID"), "{output}");
    assert_eq!(action_count(&requests, "configure"), 0, "{requests}");
    assert_eq!(action_count(&requests, "prepare"), 1, "{requests}");
    assert_eq!(action_count(&requests, "verify"), 0, "{requests}");
}

/// The shortcut reuses the challenge `/auth` prefetched and configures aliyun + RAM role.
#[test]
fn raw_cli_auth_sysom_shortcut_reuses_prefetched_challenge() {
    let (output, requests) = run_auth_menu_flow(
        "auth-sysom-shortcut",
        SAVED_NONE,
        ECS_PREPARE,
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            ("+ Add new provider", b"\n".as_slice()),
            ("Auth configured", b"".as_slice()),
        ],
    );

    let compact = compact_terminal_words(&output);
    // The first verification is ready, so no naming or authorization instructions are shown.
    assert!(!compact.contains("Provider ID"), "{output}");
    assert!(!compact.contains("ECS Instance ID"), "{output}");
    assert!(!compact.contains("i-fake-ecs-1"), "{output}");
    assert!(!compact.contains("alinux.console.aliyun.com"), "{output}");
    assert!(!compact.contains("QR"), "{output}");
    assert!(compact.contains("Auth configured"), "{output}");
    // The ECS metadata service is probed once, when `/auth` builds the menu.
    assert_eq!(action_count(&requests, "prepare"), 1, "{requests}");
    assert_eq!(action_count(&requests, "verify"), 1, "{requests}");
    assert_eq!(action_count(&requests, "configure"), 1, "{requests}");
    let configure = requests
        .lines()
        .find(|line| line.contains(r#""action":"configure""#))
        .unwrap_or_else(|| panic!("expected configure request: {requests}"));
    assert!(
        configure.contains(r#""provider_type":"aliyun""#),
        "{configure}"
    );
    assert!(
        configure.contains(r#""provider_id":"aliyun""#),
        "{configure}"
    );
    assert!(
        configure.contains(r#""auth_source":"ecs_ram_role""#),
        "{configure}"
    );
    // No manual credential ever entered the flow, so none may be persisted.
    assert!(!configure.contains("access_key_id"), "{configure}");
    assert!(!configure.contains("security_token"), "{configure}");
}

/// A failing `auth.prepare` is a missing recommendation, not a broken `/auth`.
#[test]
fn raw_cli_auth_prepare_failure_falls_back_to_the_existing_menu() {
    let (output, requests) = run_auth_menu_flow(
        "auth-prepare-failure",
        SAVED_DASHSCOPE,
        FAILING_PREPARE,
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            ("+ Add new provider", b"".as_slice()),
        ],
    );

    let compact = compact_terminal_words(&output);
    assert!(compact.contains("Provider Management"), "{output}");
    assert!(compact.contains("> [1] * [active] DashScope"), "{output}");
    assert!(compact.contains("[2] + Add new provider"), "{output}");
    assert!(!compact.contains("SysOM"), "{output}");
    assert!(!compact.contains("Auth unavailable"), "{output}");
    assert_eq!(action_count(&requests, "prepare"), 1, "{requests}");
}

/// #1760: ESC anywhere in the form used to abandon `/auth` outright, so one mistyped field cost
/// every field before it. It now walks back one prompt at a time and only cancels at the picker.
#[test]
fn raw_cli_auth_esc_walks_back_through_the_form_before_cancelling() {
    let (output, requests) = run_auth_menu_flow(
        "auth-esc-back",
        SAVED_NONE,
        MANUAL_PREPARE,
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            (
                "Authentication Required",
                b"\x1b[C\x1b[C\x1b[C\x1b[C\n".as_slice(),
            ),
            ("Enter Base URL", b"https://example.invalid/v1\n".as_slice()),
            // ESC on API Key returns to Base URL, which still carries the value just submitted.
            ("Enter API Key", b"\x1b".as_slice()),
            ("Enter Base URL", b"\x1b".as_slice()),
            // Back at the picker a further ESC is the one that ends the flow.
            ("Authentication Required", b"\x1b".as_slice()),
            ("Auth cancelled", b"".as_slice()),
        ],
    );

    let compact = compact_terminal_words(&output);
    // Each re-rendered prompt offers the value already submitted for it, not an empty field.
    assert!(
        compact.contains("> https://example.invalid/v1"),
        "stepping back lost the submitted Base URL: {output}"
    );
    assert!(
        !compact.contains("Provider ID"),
        "first-time setup must not step back onto the hidden Provider ID: {output}"
    );
    // The picker reopens on the template the form belonged to.
    assert!(compact.contains("> [5] OpenAI Compatible"), "{output}");
    // Only the last ESC cancels; the two before it are back-navigation.
    assert_eq!(
        count_occurrences(&compact, "Auth cancelled"),
        1,
        "an intermediate ESC cancelled the flow: {output}"
    );
    assert_eq!(action_count(&requests, "configure"), 0, "{requests}");
}

#[test]
fn raw_cli_auth_esc_preserves_picker_focus_for_the_next_arrow() {
    let (output, requests) = run_auth_menu_flow(
        "auth-esc-picker-focus",
        SAVED_NONE,
        MANUAL_PREPARE,
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            ("Authentication Required", b"\x1b[C\x1b[C\n".as_slice()),
            ("Enter API Key", b"\x1b".as_slice()),
            ("Authentication Required", b"\x1b[B".as_slice()),
            ("> [4] DashScope", b"\x1b".as_slice()),
            ("Auth cancelled", b"".as_slice()),
        ],
    );

    let compact = compact_terminal_words(&output);
    assert!(compact.contains("> [3] Token Plan"), "{output}");
    assert!(compact.contains("> [4] DashScope"), "{output}");
    assert!(!compact.contains("Provider ID"), "{output}");
    assert_eq!(action_count(&requests, "configure"), 0, "{requests}");
}

/// Teaching ESC to step back must not take away the interrupt: Ctrl+C still abandons the form in
/// one keystroke without advancing to the next field.
#[test]
fn raw_cli_auth_ctrl_c_mid_form_abandons_the_flow() {
    let (output, requests) = run_auth_menu_flow(
        "auth-ctrl-c-abort",
        SAVED_NONE,
        MANUAL_PREPARE,
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            (
                "Authentication Required",
                b"\x1b[C\x1b[C\x1b[C\x1b[C\n".as_slice(),
            ),
            ("Enter Base URL", b"\x03".as_slice()),
            ("Auth cancelled", b"".as_slice()),
        ],
    );

    let compact = compact_terminal_words(&output);
    assert!(compact.contains("Auth cancelled"), "{output}");
    assert_eq!(count_occurrences(&compact, "Auth cancelled"), 1, "{output}");
    assert!(!compact.contains("Provider ID"), "{output}");
    // A single Ctrl+C is enough: the form is gone, not one prompt further back.
    assert!(
        !compact.contains("Enter API Key"),
        "Ctrl+C stepped through the form instead of abandoning it: {output}"
    );
    assert_eq!(action_count(&requests, "configure"), 0, "{requests}");
}

/// An edit steps back to the action menu it came from, never onto Provider ID: that field names
/// the config being edited, so an edit of it could not take effect.
#[test]
fn raw_cli_auth_esc_on_an_edit_returns_to_the_provider_action_menu() {
    let (output, requests) = run_auth_menu_flow(
        "auth-esc-back-edit",
        SAVED_DASHSCOPE,
        MANUAL_PREPARE,
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            ("+ Add new provider", b"\n".as_slice()),
            ("Edit configuration", b"\n".as_slice()),
            // API Key is the first field an edit may change, so ESC leaves the form entirely.
            ("Edit API Key", b"\x1b".as_slice()),
            ("Edit configuration", b"\x1b".as_slice()),
            ("Auth cancelled", b"".as_slice()),
        ],
    );

    let compact = compact_terminal_words(&output);
    // The action menu is rendered twice: on the way in, and again after ESC.
    assert_eq!(
        count_occurrences(&compact, "Edit configuration"),
        2,
        "ESC did not return to the provider action menu: {output}"
    );
    assert!(
        !compact.contains("Provider ID"),
        "an edit stepped back onto the Provider ID field: {output}"
    );
    assert_eq!(action_count(&requests, "configure"), 0, "{requests}");
}

/// Manual Aliyun AK/SK stays available off ECS, with the secret fields still masked.
#[test]
fn raw_cli_auth_non_ecs_aliyun_falls_back_to_manual_keys() {
    let (output, requests) = run_auth_menu_flow(
        "auth-non-ecs-aliyun-manual",
        SAVED_NONE,
        MANUAL_PREPARE,
        &[
            ("cosh-osc$", b"/auth\n".as_slice()),
            ("Authentication Required", b"\n".as_slice()),
            ("Enter Access Key ID", b"AK-TEST-VALUE\n".as_slice()),
            ("Enter Access Key Secret", b"".as_slice()),
        ],
    );

    let compact = compact_terminal_words(&output);
    assert!(compact.contains("Enter Access Key ID"), "{output}");
    assert!(compact.contains("Enter Access Key Secret"), "{output}");
    assert!(!compact.contains("ECS Instance ID"), "{output}");
    assert!(!compact.contains("Provider ID"), "{output}");
    // Secret fields are echoed as bullets, never as the typed key.
    assert!(!compact.contains("AK-TEST-VALUE"), "{output}");
    assert!(compact.contains('\u{2022}'), "{output}");
    // The successful startup result is reused after selecting the template.
    assert_eq!(action_count(&requests, "prepare"), 1, "{requests}");
}
