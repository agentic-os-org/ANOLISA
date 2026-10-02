_cosh_command_has_secret() {
  local jwt_pattern='(^|[^A-Za-z0-9_])eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}([^A-Za-z0-9_]|$)'
  if [[ "$1" =~ $jwt_pattern ]]; then
    return 0
  fi
  local lower="${(L)1}"
  case "$lower" in
    *"-----begin "*"private key-----"*)
      return 0
      ;;
  esac
  # ASCII boundaries allow credentials next to CJK text without matching inside
  # identifiers such as npm_package_version or shf_test.
  local bearer_pattern='(^|[^a-z0-9_])bearer[[:space:]]+[a-z0-9._~+/=-]+([^a-z0-9_]|$)'
  local url_password_pattern='[a-z][a-z0-9+.-]*://[^/[:space:]:@]*:[^@/[:space:]]+@'
  local github_pattern='(^|[^a-z0-9_])(gh[pousr]_[a-z0-9_]{20,}|github_pat_[a-z0-9_]{20,})([^a-z0-9_]|$)'
  # A five-character sk- suffix needs a digit so short keys stay private
  # without classifying the sk-hynix product name as a credential.
  local short_sk_pattern='([0-9][a-z0-9_-]{4}|[a-z0-9_-][0-9][a-z0-9_-]{3}|[a-z0-9_-]{2}[0-9][a-z0-9_-]{2}|[a-z0-9_-]{3}[0-9][a-z0-9_-]|[a-z0-9_-]{4}[0-9])'
  local opaque_pattern="(^|[^a-z0-9_])(sk-([a-z0-9_-]{6,}|${short_sk_pattern})|sk_(live|test)_[a-z0-9]{10,}|glpat-[a-z0-9_-]{10,}|npm_[a-z0-9]{20,}|hf_[a-z0-9]{20,}|aiza[a-z0-9_-]{20,}|xox[a-z]-[a-z0-9-]{10,})([^a-z0-9_]|$)"
  local alibaba_pattern='(^|[^a-z0-9_])ltai[a-z0-9]{12,32}([^a-z0-9_]|$)'
  local aws_pattern='(^|[^a-z0-9_])(akia|asia)[a-z0-9]{16}([^a-z0-9_]|$)'
  if [[ "$lower" =~ $bearer_pattern
     || "$lower" =~ $url_password_pattern
     || "$lower" =~ $github_pattern
     || "$lower" =~ $opaque_pattern
     || "$lower" =~ $alibaba_pattern
     || "$lower" =~ $aws_pattern ]]; then
    return 0
  fi

  # Shell gating protects native history; Rust separately redacts persisted evidence.
  local canonical_assignment_key
  canonical_assignment_key='(alibaba[_-]?cloud[_-]?access[_-]?key[_-]?id|'
  canonical_assignment_key+='aws[_-]?access[_-]?key[_-]?id|access[_-]?key[_-]?id|'
  canonical_assignment_key+='aws[_-]?secret[_-]?access[_-]?key|access[_-]?key[_-]?secret|'
  canonical_assignment_key+='dashscope[_-]?api[_-]?key|openai[_-]?api[_-]?key|'
  canonical_assignment_key+='client[_-]?secret|security[_-]?token|refresh[_-]?token|'
  canonical_assignment_key+='access[_-]?token|github[_-]?token|id[_-]?token|'
  canonical_assignment_key+='password|passphrase|passwd|api[_-]?key|apikey|token|secret|'
  canonical_assignment_key+='authorization)'
  local assignment_value="['\"]?[^[:space:]]"
  # Match canonical suffixes across complete shell assignment identifiers.
  local canonical_assignment_pattern="(^|[^a-z0-9_-])['\"]?([a-z_][a-z0-9_]*)?${canonical_assignment_key}['\"]?[[:space:]]*(=|:)[[:space:]]*${assignment_value}"
  local canonical_flag_pattern="(^|[^a-z0-9_-])--${canonical_assignment_key}([[:space:]]+|=)[[:space:]]*${assignment_value}"
  local cookie_key='(cookie|set[_-]?cookie)'
  local cookie_flag_pattern="(^|[^a-z0-9_-])--${cookie_key}([[:space:]]+|=)[[:space:]]*[^[:space:]]"
  local cookie_assignment_pattern="(^|[^a-z0-9_-])['\"]?${cookie_key}['\"]?[[:space:]]*=[[:space:]]*[^[:space:]]"
  local cookie_json_pattern="(^|[^a-z0-9_-])['\"]${cookie_key}['\"][[:space:]]*:[[:space:]]*['\"][^'\"]+['\"]"
  local cookie_header_pattern="(^|[[:space:]'\"])${cookie_key}[[:space:]]*:[[:space:]]*[^=[:space:];,]+[[:space:]]*=[^[:space:]]*"
  local attached_cookie_header_pattern="(^|[[:space:]])(-[[:alnum:]#]*h|--header=)${cookie_key}[[:space:]]*:[[:space:]]*[^=[:space:];,]+[[:space:]]*=[^[:space:]]*"
  local dynamic_cookie_header_pattern='(\$|`)[^;&|]*'
  dynamic_cookie_header_pattern+="${cookie_key}[[:space:]]*:[[:space:]]*[^=[:space:];,]+[[:space:]]*=[^[:space:]]*"
  # Operators inside a closed substitution do not terminate its outer word.
  local dynamic_expansion_cookie_header_pattern='(\$\{.*\}|\$\(.*\)|`[^`]*`)[^[:space:];&|]*'
  dynamic_expansion_cookie_header_pattern+="${cookie_key}[[:space:]]*:[[:space:]]*[^=[:space:];,]+[[:space:]]*=[^[:space:]]*"
  # Quote removal and backslash escapes can construct a static Cookie prefix
  # even when the raw command does not contain a contiguous `Cookie:` token.
  local static_cookie_input="${lower//\\/}"
  static_cookie_input="${static_cookie_input//\'/}"
  static_cookie_input="${static_cookie_input//\"/}"
  if [[ "$lower" =~ $canonical_assignment_pattern
     || "$lower" =~ $canonical_flag_pattern
     || "$lower" =~ $cookie_flag_pattern
     || "$lower" =~ $cookie_assignment_pattern
     || "$lower" =~ $cookie_json_pattern
     || "$static_cookie_input" =~ $cookie_header_pattern
     || "$static_cookie_input" =~ $attached_cookie_header_pattern
     || "$static_cookie_input" =~ $dynamic_cookie_header_pattern
     || "$static_cookie_input" =~ $dynamic_expansion_cookie_header_pattern ]]; then
    return 0
  fi
  return 1
}
