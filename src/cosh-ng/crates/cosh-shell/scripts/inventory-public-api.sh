#!/usr/bin/env bash
set -u

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
crate_dir="$(cd "$script_dir/.." && pwd)"
repo_root="$(cd "$crate_dir/../.." && pwd)"

cd "$repo_root" || exit 2

if ! command -v rg >/dev/null 2>&1; then
  echo "error: rg is required" >&2
  exit 2
fi

classification_for_symbol() {
  case "$1" in
    config|load_config|parse_language_setting|resolve_language_setting|write_user_language_config|language_config_status|CoshConfig|Language|LanguageConfigStatus|I18n|MessageId)
      echo "stable-runtime-api"
      ;;
    adapter|adapter_for_kind|AdapterInstance|AdapterKind|AgentAdapter|AuthFieldInfo|AuthProviderInfo|AuthResponse)
      echo "support-api-review-before-freeze"
      ;;
    agent|types|tools|parser|journal|ledger|raw_input|shell_host|evidence|hooks|ui|render_transcript|default_builtin_hooks|govern_agent_events|govern_agent_events_with_language|GovernanceOutput|HookInput|HookMatcher|HookTrigger|FindingSeverity|HookFinding|classify_executed_command_outcome|classify_exit|classify_shell_handoff_command_outcome|first_program_token|CommandOutcome|ExitCodeCategory|active_slash_commands|active_slash_hint_commands|exact_slash_control_commands|slash_command_registry|visible_slash_commands|SlashCommandSpec|SlashCommandState)
      echo "internal-migration-surface"
      ;;
    agent_render|builtin_hooks|context_window|exit_classify|governance|hook_engine|hook_types|interactive|renderer|slash_registry)
      echo "root-facade-to-remove-or-hide"
      ;;
    *)
      echo "unclassified"
      ;;
  esac
}

classification_for_owner_entry() {
  case "$1" in
    adapter::adapter_for_kind|adapter::AdapterError|adapter::AdapterInstance|adapter::AdapterKind|adapter::AgentAdapter|adapter::AgentBackendCapabilities|adapter::AgentRunHandle|adapter::AgentRunPoll|adapter::ApprovalDecision|adapter::ApprovalResponse|adapter::AuthFieldInfo|adapter::AuthProviderInfo|adapter::AuthResponse|adapter::ControlProtocolCapabilities|adapter::HostExecutedShellMetadata|adapter::HostExecutedShellResult|adapter::ShellEvidenceAction)
      echo "support-api-review-before-freeze"
      ;;
    adapter::ClaudeCodeAdapter|adapter::CoshCoreAdapter|adapter::EcsProbeTask|adapter::FakeAgentAdapter|adapter::QwenCliAdapter|adapter::ProviderCancellationArtifact|adapter::ProviderCancellationArtifactKind|adapter::ProviderCancellationArtifactStore|adapter::SessionClearFailure|adapter::SessionClearInterruption|adapter::SessionClearPlan|adapter::SessionClearResult|adapter::SessionErrorInfo|adapter::SessionHealth|adapter::SessionList|adapter::SessionManagementClient|adapter::SessionRecovery|adapter::SessionRecoveryState|adapter::SessionRuntimeState|adapter::SessionSummary)
      echo "private-candidate"
      ;;
    agent::govern_agent_events|agent::govern_agent_events_with_language|agent::GovernanceOutput|journal::read_shell_events|ledger::build_command_blocks|ledger::LedgerOutput|parser::agent_request_after_confirmation|parser::findings_from_blocks|raw_input::RawInputCapture|raw_input::RawObserverAction|raw_input::RawRelayAction|shell_host::run_line_interactive_bash|shell_host::run_raw_relay_bash|shell_host::run_raw_relay_bash_with_actions|shell_host::run_raw_relay_bash_with_actions_output_control|shell_host::run_raw_relay_bash_with_observer|shell_host::run_raw_relay_zsh_with_actions|shell_host::run_raw_relay_zsh_with_output_control|shell_host::run_scripted_bash|shell_host::run_scripted_zsh|shell_host::LineInteractiveOutput|shell_host::ScriptedInput|shell_host::ShellHostConfig|shell_host::ShellHostOutput)
      echo "internal-migration-surface"
      ;;
    types::AgentEvent|types::AgentMode|types::AgentRequest|types::CommandBlock|types::CommandOrigin|types::CommandStatus|types::CoshApprovalMode|types::Finding|types::FindingKind|types::FindingSeverity|types::GovernanceDecision|types::GovernancePolicyDecision|types::GovernedEvent|types::HookFinding|types::ImplicitPagerPolicy|types::Intervention|types::InterventionDecision|types::OutputRefs|types::ShellCaptureLifecycle|types::ShellCaptureMetadata|types::ShellCommandAuditIdentity|types::ShellEvent|types::ShellEventKind|types::ShellHandoffRequest|types::ShellRoutingMetadata|types::COMMAND_OUTPUT_REF_MAX_BYTES|types::SESSION_OUTPUT_REF_MAX_BYTES)
      echo "support-api-review-before-freeze"
      ;;
    types::AuditRecord|types::Policy|types::QuestionSelectionMode)
      echo "private-candidate"
      ;;
    *)
      echo "owner-public-surface"
      ;;
  esac
}

owner_for_symbol() {
  case "$1" in
    agent_render|renderer|render_transcript) echo "ui" ;;
    hook_engine|builtin_hooks|default_builtin_hooks) echo "hooks" ;;
    hook_types) echo "types/hooks+hooks/model" ;;
    FindingSeverity|HookFinding) echo "types/hooks" ;;
    HookInput|HookMatcher|HookTrigger) echo "hooks/model" ;;
    context_window|evidence) echo "evidence" ;;
    exit_classify|classify_executed_command_outcome|classify_exit|classify_shell_handoff_command_outcome|first_program_token|CommandOutcome|ExitCodeCategory) echo "command" ;;
    agent|governance|govern_agent_events|govern_agent_events_with_language|GovernanceOutput) echo "agent" ;;
    interactive) echo "shell_host" ;;
    slash_registry|active_slash_commands|active_slash_hint_commands|exact_slash_control_commands|slash_command_registry|visible_slash_commands|SlashCommandSpec|SlashCommandState) echo "slash" ;;
    adapter|adapter_for_kind|AdapterInstance|AdapterKind|AgentAdapter|AuthFieldInfo|AuthProviderInfo|AuthResponse) echo "adapter" ;;
    config|load_config|parse_language_setting|resolve_language_setting|write_user_language_config|language_config_status|CoshConfig|Language|LanguageConfigStatus) echo "config" ;;
    I18n|MessageId) echo "i18n" ;;
    *) echo "$1" ;;
  esac
}

print_root_row() {
  local kind="$1"
  local line="$2"
  local symbol="$3"
  local classification owner action
  classification="$(classification_for_symbol "$symbol")"
  owner="$(owner_for_symbol "$symbol")"
  case "$classification" in
    stable-runtime-api) action="keep_public;document_contract" ;;
    support-api-review-before-freeze) action="confirm_external_need_before_public_freeze" ;;
    internal-migration-surface) action="replace_internal_users_with_crate_paths_then_review_visibility" ;;
    root-facade-to-remove-or-hide) action="migrate_to_owner_module_then_remove_or_make_private" ;;
    *) action="classify_before_acceptance" ;;
  esac
  printf "%s,%s,%s,%s,%s,%s\n" "$kind" "$line" "$symbol" "$owner" "$classification" "$action"
}

print_nested_row() {
  local kind="$1"
  local line="$2"
  local symbol="$3"
  local owner="$4"
  printf "%s,%s,%s,%s,nested-public-surface,itemize_nested_facade_then_review_visibility\n" \
    "$kind" "$line" "$symbol" "$owner"
}

print_owner_entry_row() {
  local kind="$1"
  local line="$2"
  local symbol="$3"
  local owner="$4"
  local classification action
  classification="$(classification_for_owner_entry "$symbol")"
  case "$classification" in
    stable-runtime-api) action="keep_public;document_contract" ;;
    support-api-review-before-freeze) action="confirm_external_need_before_public_freeze" ;;
    internal-migration-surface) action="replace_internal_users_with_crate_paths_then_review_visibility" ;;
    private-candidate) action="make_crate_private_after_test_or_runtime_migration" ;;
    *) action="classify_before_acceptance" ;;
  esac
  printf "%s,%s,%s,%s,%s,%s\n" "$kind" "$line" "$symbol" "$owner" "$classification" "$action"
}

print_public_use_rows() {
  local file="$1"
  local owner="$2"
  local kind="$3"
  local row_printer="$4"

  awk '
    /^pub use / {
      line = NR
      statement = $0
      if ($0 ~ /;/) {
        print line ":" statement
        statement = ""
      }
      next
    }
    statement != "" {
      statement = statement " " $0
      if ($0 ~ /;/) {
        print line ":" statement
        statement = ""
      }
    }
  ' "$file" | while IFS=: read -r line text; do
    local symbols
    symbols="$(printf '%s\n' "$text" |
      sed -E 's/^pub use ([^{};]+)::([A-Za-z0-9_]+);?[[:space:]]*$/\2/; s/^pub use ([^{};]+)::\*;?[[:space:]]*$/\1::*/; s/^pub use [^{};]*\{//; s/\};?[[:space:]]*$//; s/;[[:space:]]*$//' |
      tr '{}' '  ' |
      tr ',' '\n' |
      sed -E 's/^[[:space:]]+|[[:space:]]+$//g' |
      sed '/^$/d')"
    while IFS= read -r symbol; do
      "$row_printer" "$kind" "$line" "$owner::$symbol" "$owner"
    done <<< "$symbols"
  done
}

print_nested_facade_rows() {
  local file="$1"
  local owner="$2"

  while IFS=: read -r line text; do
    local symbol
    symbol="$(printf '%s\n' "$text" | sed -E 's/pub mod ([A-Za-z0-9_]+);/\1/')"
    print_nested_row nested_pub_mod "$line" "$owner::$symbol" "$owner"
  done < <(rg -n '^pub mod ' "$file" || true)

  print_public_use_rows "$file" "$owner" nested_pub_use print_nested_row
}

print_root_pub_mod_entry_rows() {
  local owner="$1"
  local file="crates/cosh-shell/src/$owner/mod.rs"
  if [ -f "crates/cosh-shell/src/$owner/public.rs" ]; then
    file="crates/cosh-shell/src/$owner/public.rs"
  fi
  if [ ! -f "$file" ]; then
    return 0
  fi

  while IFS=: read -r line text; do
    local symbol
    symbol="$(printf '%s\n' "$text" |
      sed -E 's/^pub (struct|enum|trait|fn|const|type) ([A-Za-z0-9_]+).*/\2/')"
    print_owner_entry_row owner_pub_item "$line" "$owner::$symbol" "$owner"
  done < <(rg -n '^pub (struct|enum|trait|fn|const|type) [A-Za-z0-9_]+' "$file" || true)

  while IFS=: read -r line text; do
    local symbol
    symbol="$(printf '%s\n' "$text" | sed -E 's/pub mod ([A-Za-z0-9_]+);/\1/')"
    print_owner_entry_row owner_pub_mod "$line" "$owner::$symbol" "$owner"
  done < <(rg -n '^pub mod ' "$file" || true)

  print_public_use_rows "$file" "$owner" owner_pub_use print_owner_entry_row
}

echo "kind,line,symbol,owner,classification,next_action"

root_pub_mods="$(
  rg '^pub mod ' crates/cosh-shell/src/lib.rs |
    sed -E 's/pub mod ([A-Za-z0-9_]+);/\1/' |
    sort
)"

is_root_pub_mod() {
  local owner="$1"
  printf '%s\n' "$root_pub_mods" | grep -Fxq "$owner"
}

while IFS=: read -r line text; do
  symbol="$(printf '%s\n' "$text" | sed -E 's/pub mod ([A-Za-z0-9_]+);/\1/')"
  print_root_row pub_mod "$line" "$symbol"
done < <(rg -n '^pub mod ' crates/cosh-shell/src/lib.rs)

awk '
  /^pub use / {
    line = NR
    statement = $0
    if ($0 ~ /;/) {
      print line ":" statement
      statement = ""
    }
    next
  }
  statement != "" {
    statement = statement " " $0
    if ($0 ~ /;/) {
      print line ":" statement
      statement = ""
    }
  }
' crates/cosh-shell/src/lib.rs | while IFS=: read -r line text; do
  symbols="$(printf '%s\n' "$text" |
    sed -E 's/^pub use ([^{};]+)::([A-Za-z0-9_]+);?[[:space:]]*$/\2/; s/^pub use [^{};]*\{//; s/\};?[[:space:]]*$//; s/;[[:space:]]*$//' |
    tr '{}' '  ' |
    tr ',' '\n' |
    sed -E 's/^[[:space:]]+|[[:space:]]+$//g' |
    sed '/^$/d')"
  while IFS= read -r symbol; do
    print_root_row pub_use "$line" "$symbol"
  done <<< "$symbols"
done

if is_root_pub_mod hooks; then
  print_nested_facade_rows crates/cosh-shell/src/hooks/public.rs hooks
fi
if is_root_pub_mod ui; then
  print_nested_facade_rows crates/cosh-shell/src/ui/public.rs ui
fi
if is_root_pub_mod evidence; then
  print_nested_facade_rows crates/cosh-shell/src/evidence/public.rs evidence
fi
for owner in adapter agent config journal ledger parser raw_input shell_host tools types; do
  if is_root_pub_mod "$owner"; then
    print_root_pub_mod_entry_rows "$owner"
  fi
done
