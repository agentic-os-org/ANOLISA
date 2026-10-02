_cosh_ascii_trim() {
  local value="$1"
  while [[ "$value" == ' '* || "$value" == $'\t'* ]]; do
    value="${value#?}"
  done
  while [[ "$value" == *' ' || "$value" == *$'\t' ]]; do
    case "$value" in
      *' ') value="${value% }" ;;
      *$'\t') value="${value%$'\t'}" ;;
    esac
  done
  printf '%s' "$value"
}

_cosh_byte_at() {
  local value="$1"
  local index="$2"
  local byte="${value:$index:1}"
  [[ -n "$byte" ]] || return 1
  case "$byte" in
__COSH_HIGH_BYTE_CASES__
  esac
  printf -v _COSH_BYTE_AT_RESULT '%d' "'$byte"
}

_cosh_utf8_han_status() {
  local value="$1"
  local LC_ALL=C
  local index=0
  local length="${#value}"
  local found_han=0
  local b1 b2 b3 b4 codepoint
  local _COSH_BYTE_AT_RESULT

  case "$value" in
    *[!\ -~]*) ;;
    *) return 1 ;;
  esac

  while (( index < length )); do
    _cosh_byte_at "$value" "$index" || return 2
    b1="$_COSH_BYTE_AT_RESULT"
    if (( b1 <= 127 )); then
      (( index += 1 ))
      continue
    fi

    if (( b1 >= 194 && b1 <= 223 )); then
      _cosh_byte_at "$value" "$((index + 1))" || return 2
      b2="$_COSH_BYTE_AT_RESULT"
      (( b2 >= 128 && b2 <= 191 )) || return 2
      codepoint=$(( ((b1 & 31) << 6) | (b2 & 63) ))
      (( index += 2 ))
    elif (( b1 >= 224 && b1 <= 239 )); then
      _cosh_byte_at "$value" "$((index + 1))" || return 2
      b2="$_COSH_BYTE_AT_RESULT"
      _cosh_byte_at "$value" "$((index + 2))" || return 2
      b3="$_COSH_BYTE_AT_RESULT"
      (( b2 >= 128 && b2 <= 191 && b3 >= 128 && b3 <= 191 )) || return 2
      (( b1 != 224 || b2 >= 160 )) || return 2
      (( b1 != 237 || b2 <= 159 )) || return 2
      codepoint=$(( ((b1 & 15) << 12) | ((b2 & 63) << 6) | (b3 & 63) ))
      (( index += 3 ))
    elif (( b1 >= 240 && b1 <= 244 )); then
      _cosh_byte_at "$value" "$((index + 1))" || return 2
      b2="$_COSH_BYTE_AT_RESULT"
      _cosh_byte_at "$value" "$((index + 2))" || return 2
      b3="$_COSH_BYTE_AT_RESULT"
      _cosh_byte_at "$value" "$((index + 3))" || return 2
      b4="$_COSH_BYTE_AT_RESULT"
      (( b2 >= 128 && b2 <= 191 && b3 >= 128 && b3 <= 191 && b4 >= 128 && b4 <= 191 )) || return 2
      (( b1 != 240 || b2 >= 144 )) || return 2
      (( b1 != 244 || b2 <= 143 )) || return 2
      codepoint=$(( ((b1 & 7) << 18) | ((b2 & 63) << 12) | ((b3 & 63) << 6) | (b4 & 63) ))
      (( index += 4 ))
    else
      return 2
    fi

    if (( (codepoint >= 0x3400 && codepoint <= 0x4DBF)
       || (codepoint >= 0x4E00 && codepoint <= 0x9FFF)
       || (codepoint >= 0xF900 && codepoint <= 0xFAFF)
       || (codepoint >= 0x20000 && codepoint <= 0x323AF) )); then
      found_han=1
    fi
  done

  (( found_han == 1 )) && return 0
  return 1
}

_cosh_literal_first_word_matches() {
  local input="$1"
  local attempt_token="$2"
  local command="$3"
  local LC_ALL=C
  local quote=""
  local normalized=""
  local started=0
  local index=0
  local length="${#input}"
  local byte

  [[ -n "$input" && "$length" -le 4096 ]] || return 1
  [[ "$attempt_token" == "$command" ]] && return 0

  while (( index < length )); do
    byte="${input:$index:1}"
    (( index += 1 ))
    if [[ "$quote" == "'" ]]; then
      case "$byte" in
        "'") quote="" ;;
        *[[:cntrl:]]*) return 1 ;;
        *) normalized+="$byte"; started=1 ;;
      esac
      continue
    fi
    if [[ "$quote" == '"' ]]; then
      case "$byte" in
        '"') quote="" ;;
        '\'|'$'|'`'|*[[:cntrl:]]*) return 1 ;;
        *) normalized+="$byte"; started=1 ;;
      esac
      continue
    fi
    case "$byte" in
      ' '|$'\t') (( started == 1 )) && break ;;
      "'") quote="'"; started=1 ;;
      '"') quote='"'; started=1 ;;
      '\'|'$'|'`'|'|'|'&'|';'|'<'|'>'|'('|')'|'*'|'?'|'~'|'{'|'}'|'['|']'|*[[:cntrl:]]*)
        return 1
        ;;
      *) normalized+="$byte"; started=1 ;;
    esac
  done
  [[ -z "$quote" && -n "$normalized" && "$normalized" == "$command" ]]
}

_cosh_arguments_have_no_unquoted_expansion() {
  local input="$1"
  local LC_ALL=C
  local quote=""
  local escaped=0
  local after_first_word=0
  local index=0
  local length="${#input}"
  local byte

  while (( index < length )); do
    byte="${input:$index:1}"
    (( index += 1 ))
    if [[ "$quote" == "'" ]]; then
      [[ "$byte" == "'" ]] && quote=""
      continue
    fi
    if (( escaped == 1 )); then
      escaped=0
      continue
    fi
    case "$byte" in
      '\') escaped=1 ;;
      "'") quote="'" ;;
      '"')
        if [[ "$quote" == '"' ]]; then quote=""; elif [[ -z "$quote" ]]; then quote='"'; fi
        ;;
      ' '|$'\t') [[ -z "$quote" ]] && after_first_word=1 ;;
      '*'|'?'|'~'|'{'|'}'|'['|']')
        (( after_first_word == 1 )) && [[ -z "$quote" ]] && return 1
        ;;
    esac
  done
  (( escaped == 0 )) && [[ -z "$quote" ]]
}

_cosh_command_veto() {
  local input="$1"
  local top_token="$2"
  local top_han_status="${3:-1}"
  local scan="$input"
  local word name

  case "$top_token" in
    /*|*/*|'~/'*|command|env|sudo|exec|nohup|time|xargs)
      return 0
      ;;
  esac

  case "$scan" in
    *'?') scan="${scan%\?}" ;;
    *'？') scan="${scan%？}" ;;
  esac

  if (( top_han_status == 0 )); then
    case "$scan" in
      *[[:cntrl:]]*)
        return 0
        ;;
    esac

    local quote=""
    local escaped=0
    local index=0
    local length="${#scan}"
    local byte
    while (( index < length )); do
      byte="${scan:$index:1}"
      (( index += 1 ))
      if [[ "$quote" == "'" ]]; then
        [[ "$byte" == "'" ]] && quote=""
        continue
      fi
      if (( escaped == 1 )); then
        escaped=0
        continue
      fi
      case "$byte" in
        '\') escaped=1 ;;
        "'") quote="'" ;;
        '"')
          if [[ "$quote" == '"' ]]; then
            quote=""
          elif [[ -z "$quote" ]]; then
            quote='"'
          fi
          ;;
        '('|')')
          [[ -z "$quote" ]] && return 0
          ;;
        '|'|'&'|';'|'<'|'>')
          # command-not-found runs after parsing. A successful intercept
          # would otherwise alter the status of an executable shell list.
          [[ -z "$quote" ]] && return 0
          ;;
        '`')
          # Backticks expand inside double quotes, but are literal in single
          # quotes. Never let command substitution reach the shell.
          [[ "$quote" != "'" ]] && return 0
          ;;
        '$')
          # Only simple parameter references are safe to quote for the Agent.
          # Shell-specific expansion operators can evaluate their value before
          # command-not-found runs, so they remain shell-owned.
          if [[ "$quote" != "'" ]]; then
            case "${scan:$index:1}" in
              [A-Za-z_]) ;;
              '{')
                [[ "${scan:$index}" =~ ^\{[A-Za-z_][A-Za-z0-9_]*\} ]] || return 0
                ;;
              *) return 0 ;;
            esac
          fi
          ;;
      esac
    done
    (( escaped == 1 )) && return 0
    [[ -n "$quote" ]] && return 0
    return 1
  fi

  case "$scan" in
    *"'"*|*'"'*|*'\'*|*'|'*|*'&'*|*';'*|*'<'*|*'>'*|*'$'*|*'`'*|*'('*|*')'*|*'{'*|*'}'*|*'['*|*']'*|*'*'*|*'?'*|*'？'*|*'~'*|*[[:cntrl:]]*)
      return 0
      ;;
  esac

  if [[ -n "${ZSH_VERSION:-}" ]]; then
    set -- ${=scan}
  else
    set -- $scan
  fi
  for word in "$@"; do
    case "$word" in
      -*) return 0 ;;
      [A-Za-z_]*=*)
        name="${word%%=*}"
        case "$name" in
          ''|*[!A-Za-z0-9_]*) ;;
          *) return 0 ;;
        esac
        ;;
    esac
  done
  return 1
}

_cosh_request_verb() {
  case "$1" in
    [Ee][Xx][Pp][Ll][Aa][Ii][Nn]|[Cc][Hh][Ee][Cc][Kk]|[Ss][Hh][Oo][Ww]|[Tt][Ee][Ll][Ll]|[Hh][Ee][Ll][Pp]|[Aa][Nn][Aa][Ll][Yy][Zz][Ee]|[Aa][Nn][Aa][Ll][Yy][Ss][Ee]|[Rr][Ee][Vv][Ii][Ee][Ww]|[Ff][Ii][Xx]|[Ss][Uu][Mm][Mm][Aa][Rr][Ii][Zz][Ee]|[Ss][Uu][Mm][Mm][Aa][Rr][Ii][Ss][Ee]|[Ii][Nn][Ss][Pp][Ee][Cc][Tt]|[Dd][Ii][Aa][Gg][Nn][Oo][Ss][Ee]|[Dd][Ee][Bb][Uu][Gg]|[Cc][Oo][Mm][Pp][Aa][Rr][Ee]|[Dd][Ee][Ss][Cc][Rr][Ii][Bb][Ee]|[Ll][Ii][Ss][Tt]|[Tt][Rr][Aa][Nn][Ss][Ll][Aa][Tt][Ee]|[Gg][Ee][Nn][Ee][Rr][Aa][Tt][Ee]|[Rr][Uu][Nn]|[Ff][Ii][Nn][Dd]|[Ss][Ee][Aa][Rr][Cc][Hh]|[Oo][Pp][Ee][Nn]|[Rr][Ee][Aa][Dd]|[Ee][Dd][Ii][Tt]|[Cc][Rr][Ee][Aa][Tt][Ee]|[Uu][Pp][Dd][Aa][Tt][Ee]|[Ww][Rr][Ii][Tt][Ee]|[Rr][Ee][Mm][Oo][Vv][Ee]|[Dd][Ee][Ll][Ee][Tt][Ee]|[Ii][Nn][Ss][Tt][Aa][Ll][Ll]|[Uu][Nn][Ii][Nn][Ss][Tt][Aa][Ll][Ll]|[Cc][Oo][Nn][Ff][Ii][Gg][Uu][Rr][Ee]|[Ss][Ee][Tt][Uu][Pp]|[Ss][Tt][Aa][Rr][Tt]|[Ss][Tt][Oo][Pp]|[Rr][Ee][Ss][Tt][Aa][Rr][Tt]|[Rr][Ee][Ll][Oo][Aa][Dd]|[Rr][Ee][Ss][Ee][Tt]|[Bb][Uu][Ii][Ll][Dd]|[Dd][Ee][Pp][Ll][Oo][Yy]|[Tt][Ee][Ss][Tt]|[Vv][Aa][Ll][Ii][Dd][Aa][Tt][Ee]|[Vv][Ee][Rr][Ii][Ff][Yy]|[Ii][Nn][Vv][Ee][Ss][Tt][Ii][Gg][Aa][Tt][Ee]|[Tt][Rr][Oo][Uu][Bb][Ll][Ee][Ss][Hh][Oo][Oo][Tt]|[Mm][Oo][Nn][Ii][Tt][Oo][Rr]|[Oo][Pp][Tt][Ii][Mm][Ii][Zz][Ee]|[Oo][Pp][Tt][Ii][Mm][Ii][Ss][Ee]|[Cc][Ll][Ee][Aa][Nn]|[Ff][Oo][Rr][Mm][Aa][Tt]|[Cc][Oo][Nn][Vv][Ee][Rr][Tt]|[Dd][Oo][Ww][Nn][Ll][Oo][Aa][Dd]|[Uu][Pp][Ll][Oo][Aa][Dd])
      return 0
      ;;
  esac
  return 1
}

_cosh_classify_missing() {
  local LC_ALL=C
  local original
  original="$(_cosh_ascii_trim "$1")"
  local top_token="$2"
  local original_han_status top_han_status had_question=0 polite=0
  local IFS=$' \t\n'

  if [[ -z "$original" || ${#original} -gt 4096 ]]; then
    printf '%s' "unsafe"
    return 0
  fi
  _cosh_utf8_han_status "$original"
  original_han_status=$?
  if (( original_han_status == 2 )); then
    printf '%s' "unsafe"
    return 0
  fi

  _cosh_utf8_han_status "$top_token"
  top_han_status=$?
  if (( top_han_status == 2 )); then
    printf '%s' "unsafe"
    return 0
  fi

  if _cosh_command_veto "$original" "$top_token" "$top_han_status"; then
    printf '%s' "command"
    return 0
  fi

  if (( original_han_status == 0 )); then
    printf '%s' "natural_language"
    return 0
  fi

  case "$original" in
    *'?') original="${original%\?}"; had_question=1 ;;
    *'？') original="${original%？}"; had_question=1 ;;
  esac
  if [[ -n "${ZSH_VERSION:-}" ]]; then
    set -- ${=original}
  else
    set -- $original
  fi

  while (( $# > 0 )); do
    case "$1" in
      [Pp][Ll][Ee][Aa][Ss][Ee]|[Kk][Ii][Nn][Dd][Ll][Yy])
        polite=1
        shift
        ;;
      [Jj][Uu][Ss][Tt]|[Ss][Ii][Mm][Pp][Ll][Yy]|[Mm][Aa][Yy][Bb][Ee]|[Pp][Ee][Rr][Hh][Aa][Pp][Ss])
        shift
        ;;
      *)
        break
        ;;
    esac
  done

  local count=$#
  local first="${1:-}"
  local second="${2:-}"
  local third="${3:-}"

  if (( count == 0 )); then
    printf '%s' "ambiguous"
    return 0
  fi
  if (( polite == 1 )); then
    printf '%s' "natural_language"
    return 0
  fi

  case "$first" in
    [Ww][Hh][Oo]|[Ww][Hh][Aa][Tt]|[Ww][Hh][Yy]|[Ww][Hh][Ee][Rr][Ee]|[Ww][Hh][Ee][Nn]|[Hh][Oo][Ww]|[Ww][Hh][Ii][Cc][Hh])
      (( count >= 2 || had_question == 1 )) && { printf '%s' "natural_language"; return 0; }
      ;;
    [Cc][Aa][Nn]|[Cc][Oo][Uu][Ll][Dd]|[Ww][Oo][Uu][Ll][Dd]|[Ss][Hh][Oo][Uu][Ll][Dd]|[Ww][Ii][Ll][Ll]|[Mm][Aa][Yy]|[Mm][Ii][Gg][Hh][Tt]|[Mm][Uu][Ss][Tt]|[Ii][Ss]|[Aa][Rr][Ee]|[Aa][Mm]|[Ww][Aa][Ss]|[Ww][Ee][Rr][Ee]|[Dd][Oo]|[Dd][Oo][Ee][Ss]|[Dd][Ii][Dd]|[Hh][Aa][Ss]|[Hh][Aa][Vv][Ee]|[Hh][Aa][Dd])
      (( count >= 3 )) && { printf '%s' "natural_language"; return 0; }
      ;;
    [Ii]|[Ww][Ee])
      if (( count >= 3 )); then
        case "$second" in
          [Nn][Ee][Ee][Dd]|[Ww][Aa][Nn][Tt]|[Ww][Oo][Uu][Ll][Dd]|[Aa][Mm]|[Aa][Rr][Ee]|[Hh][Aa][Vv][Ee]|[Tt][Hh][Ii][Nn][Kk]|[Bb][Ee][Ll][Ii][Ee][Vv][Ee]|[Hh][Oo][Pp][Ee]|[Cc][Aa][Nn]|[Cc][Aa][Nn][Nn][Oo][Tt]|[Ss][Hh][Oo][Uu][Ll][Dd]|[Mm][Uu][Ss][Tt])
            printf '%s' "natural_language"
            return 0
            ;;
        esac
      fi
      ;;
    [Yy][Oo][Uu])
      if (( count >= 3 )); then
        case "$second" in
          [Cc][Aa][Nn]|[Cc][Oo][Uu][Ll][Dd]|[Ww][Oo][Uu][Ll][Dd]|[Ss][Hh][Oo][Uu][Ll][Dd]|[Nn][Ee][Ee][Dd]|[Hh][Aa][Vv][Ee]|[Aa][Rr][Ee])
            printf '%s' "natural_language"
            return 0
            ;;
        esac
      fi
      ;;
    [Tt][Hh][Ii][Ss]|[Tt][Hh][Aa][Tt]|[Ii][Tt]|[Tt][Hh][Ee][Ss][Ee]|[Tt][Hh][Oo][Ss][Ee])
      if (( count >= 2 )); then
        case "$second" in
          [Ii][Ss]|[Aa][Rr][Ee]|[Ww][Aa][Ss]|[Ww][Ee][Rr][Ee]|[Ll][Oo][Oo][Kk][Ss]|[Ss][Ee][Ee][Mm][Ss]|[Ww][Oo][Rr][Kk][Ss]|[Ff][Aa][Ii][Ll][Ee][Dd]|[Bb][Rr][Oo][Kk][Ee]|[Dd][Oo][Ee][Ss]|[Dd][Ii][Dd])
            printf '%s' "natural_language"
            return 0
            ;;
        esac
      fi
      ;;
    [Tt][Hh][Ee][Rr][Ee])
      if (( count >= 3 )); then
        case "$second" in
          [Ii][Ss]|[Aa][Rr][Ee]|[Ww][Aa][Ss]|[Ww][Ee][Rr][Ee]|[Hh][Aa][Ss]|[Hh][Aa][Vv][Ee])
            printf '%s' "natural_language"
            return 0
            ;;
        esac
      fi
      ;;
    [Nn][Ee][Ee][Dd]|[Ww][Aa][Nn][Tt])
      (( count >= 2 )) && { printf '%s' "natural_language"; return 0; }
      ;;
    [Aa][Nn][Yy])
      if (( count >= 2 )); then
        case "$second" in
          [Ii][Dd][Ee][Aa][Ss]|[Tt][Hh][Oo][Uu][Gg][Hh][Tt][Ss]|[Ss][Uu][Gg][Gg][Ee][Ss][Tt][Ii][Oo][Nn][Ss]|[Hh][Ee][Ll][Pp])
            printf '%s' "natural_language"
            return 0
            ;;
        esac
      fi
      ;;
  esac

  if (( count >= 3 )); then
    case "$second" in
      [Ii][Ss]|[Aa][Rr][Ee]|[Ww][Aa][Ss]|[Ww][Ee][Rr][Ee]|[Ll][Oo][Oo][Kk][Ss]|[Ss][Ee][Ee][Mm][Ss]|[Ww][Oo][Rr][Kk][Ss]|[Ff][Aa][Ii][Ll][Ss]|[Ff][Aa][Ii][Ll][Ee][Dd]|[Kk][Ee][Ee][Pp][Ss]|[Hh][Aa][Ss]|[Hh][Aa][Vv][Ee]|[Dd][Oo][Ee][Ss]|[Dd][Ii][Dd])
        printf '%s' "natural_language"
        return 0
        ;;
      [Nn][Oo][Tt])
        case "$third" in
          [Ww][Oo][Rr][Kk][Ii][Nn][Gg]|[Rr][Uu][Nn][Nn][Ii][Nn][Gg]|[Rr][Ee][Ss][Pp][Oo][Nn][Dd][Ii][Nn][Gg]|[Aa][Vv][Aa][Ii][Ll][Aa][Bb][Ll][Ee]|[Rr][Ee][Aa][Dd][Yy])
            printf '%s' "natural_language"
            return 0
            ;;
        esac
        ;;
    esac

    case "$first" in
      [Tt][Hh][Ee]|[Aa]|[Aa][Nn]|[Mm][Yy]|[Oo][Uu][Rr]|[Yy][Oo][Uu][Rr])
        case "$third" in
          [Ii][Ss]|[Aa][Rr][Ee]|[Ww][Aa][Ss]|[Ww][Ee][Rr][Ee]|[Ll][Oo][Oo][Kk][Ss]|[Ss][Ee][Ee][Mm][Ss]|[Ww][Oo][Rr][Kk][Ss]|[Ff][Aa][Ii][Ll][Ss]|[Ff][Aa][Ii][Ll][Ee][Dd]|[Bb][Rr][Oo][Kk][Ee]|[Hh][Aa][Ss]|[Hh][Aa][Vv][Ee]|[Dd][Oo][Ee][Ss]|[Dd][Ii][Dd])
            printf '%s' "natural_language"
            return 0
            ;;
        esac
        ;;
    esac
  fi

  if _cosh_request_verb "$first" && (( count >= 2 )); then
    printf '%s' "natural_language"
    return 0
  fi

  case "$first:$second" in
    [Dd][Oo]:[Ii][Tt]|[Dd][Oo]:[Tt][Hh][Ii][Ss]|[Dd][Oo]:[Tt][Hh][Aa][Tt]|[Dd][Oo]:[Ss][Oo]|[Gg][Oo]:[Aa][Hh][Ee][Aa][Dd]|[Gg][Oo]:[Oo][Nn]|[Gg][Oo]:[Bb][Aa][Cc][Kk]|[Tt][Rr][Yy]:[Aa][Gg][Aa][Ii][Nn]|[Tt][Rr][Yy]:[Ii][Tt]|[Tt][Rr][Yy]:[Tt][Hh][Ii][Ss]|[Tt][Rr][Yy]:[Tt][Hh][Aa][Tt]|[Kk][Ee][Ee][Pp]:[Gg][Oo][Ii][Nn][Gg]|[Kk][Ee][Ee][Pp]:[Tt][Rr][Yy][Ii][Nn][Gg]|[Kk][Ee][Ee][Pp]:[Ww][Oo][Rr][Kk][Ii][Nn][Gg]|[Cc][Aa][Rr][Rr][Yy]:[Oo][Nn]|[Nn][Ee][Vv][Ee][Rr]:[Mm][Ii][Nn][Dd]|[Ff][Oo][Rr][Gg][Ee][Tt]:[Ii][Tt]|[Ff][Oo][Rr][Gg][Ee][Tt]:[Tt][Hh][Aa][Tt]|[Tt][Hh][Aa][Nn][Kk]:[Yy][Oo][Uu]|[Tt][Hh][Aa][Nn][Kk][Ss]:[Aa][Gg][Aa][Ii][Nn]|[Tt][Hh][Aa][Nn][Kk][Ss]:[Aa][Nn][Yy][Ww][Aa][Yy]|[Gg][Oo][Oo][Dd]:[Mm][Oo][Rr][Nn][Ii][Nn][Gg]|[Gg][Oo][Oo][Dd]:[Aa][Ff][Tt][Ee][Rr][Nn][Oo][Oo][Nn]|[Gg][Oo][Oo][Dd]:[Ee][Vv][Ee][Nn][Ii][Nn][Gg]|[Hh][Ee][Ll][Ll][Oo]:[Tt][Hh][Ee][Rr][Ee]|[Hh][Ii]:[Tt][Hh][Ee][Rr][Ee]|[Hh][Ee][Yy]:[Tt][Hh][Ee][Rr][Ee]|[Yy][Ee][Ss]:[Pp][Ll][Ee][Aa][Ss][Ee]|[Nn][Oo][Tt]:[Ww][Oo][Rr][Kk][Ii][Nn][Gg]|[Ll][Ee][Tt]:[Uu][Ss]|[Ss][Oo][Uu][Nn][Dd][Ss]:[Gg][Oo][Oo][Dd]|[Ll][Oo][Oo][Kk][Ss]:[Gg][Oo][Oo][Dd]|[Nn][Oo]:[Tt][Hh][Aa][Nn][Kk][Ss]|[Nn][Oo]:[Pp][Rr][Oo][Bb][Ll][Ee][Mm]|[Nn][Oo]:[Ww][Oo][Rr][Rr][Ii][Ee][Ss])
      printf '%s' "natural_language"
      return 0
      ;;
  esac

  printf '%s' "ambiguous"
}
