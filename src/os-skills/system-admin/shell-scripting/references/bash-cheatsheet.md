<!-- Part of the shell-scripting AbsolutelySkilled skill. Load this file when
     writing bash scripts and needing quick reference for built-ins, parameter
     expansion syntax, test operators, or special variables. -->

# Bash Cheatsheet

Quick reference for writing production bash scripts. Covers the constructs used
most often in real automation work.

---

## Special variables

| Variable | Meaning |
|---|---|
| `$0` | Script name (path as invoked) |
| `$1` .. `$9` | Positional arguments |
| `$@` | All positional args as separate words (always quote: `"$@"`) |
| `$*` | All positional args as a single string (rarely useful) |
| `$#` | Number of positional arguments |
| `$?` | Exit code of the last command |
| `$$` | PID of the current shell |
| `$!` | PID of the last background job |
| `$_` | Last argument of the previous command |
| `BASH_SOURCE[0]` | Path of the currently executing script file |
| `LINENO` | Current line number in the script |
| `FUNCNAME[0]` | Name of the current function |
| `IFS` | Internal Field Separator (default: space/tab/newline) |

---

## Parameter expansion

### Basic

| Syntax | Result |
|---|---|
| `${var}` | Value of `var` (braces prevent ambiguity) |
| `${var:-default}` | Value of `var`, or `default` if unset or empty |
| `${var:=default}` | Value of `var`; also assigns `default` if unset or empty |
| `${var:?message}` | Value of `var`; exits with `message` if unset or empty |
| `${var:+other}` | `other` if `var` is set and non-empty; else empty string |

### Substring

| Syntax | Result |
|---|---|
| `${var:offset}` | Substring from `offset` to end |
| `${var:offset:length}` | Substring of `length` starting at `offset` |
| `${#var}` | Length of `var` |

### Pattern removal

| Syntax | Result |
|---|---|
| `${var#pattern}` | Remove shortest prefix matching `pattern` |
| `${var##pattern}` | Remove longest prefix matching `pattern` |
| `${var%pattern}` | Remove shortest suffix matching `pattern` |
| `${var%%pattern}` | Remove longest suffix matching `pattern` |

Common uses:
```bash
"${path##*/}"     # basename equivalent
"${path%/*}"      # dirname equivalent
"${file%.txt}"    # strip .txt extension
```

### Substitution and case

| Syntax | Result |
|---|---|
| `${var/pattern/replace}` | Replace first match of `pattern` with `replace` |
| `${var//pattern/replace}` | Replace all matches |
| `${var/#pattern/replace}` | Replace if `pattern` matches at start |
| `${var/%pattern/replace}` | Replace if `pattern` matches at end |
| `${var,,}` | Convert all characters to lowercase (bash 4+) |
| `${var^^}` | Convert all characters to uppercase (bash 4+) |
| `${var^}` | Capitalise first character (bash 4+) |

---

## Test operators

### File tests (`[[ -X file ]]`)

| Operator | True if |
|---|---|
| `-e file` | File exists (any type) |
| `-f file` | Regular file exists |
| `-d file` | Directory exists |
| `-L file` | Symbolic link exists |
| `-r file` | File is readable |
| `-w file` | File is writable |
| `-x file` | File is executable |
| `-s file` | File exists and is non-empty |
| `-z file` | File is zero bytes |
| `f1 -nt f2` | `f1` is newer than `f2` |
| `f1 -ot f2` | `f1` is older than `f2` |

### String tests

| Operator | True if |
|---|---|
| `-z "$s"` | String is empty |
| `-n "$s"` | String is non-empty |
| `"$a" == "$b"` | Strings are equal |
| `"$a" != "$b"` | Strings are not equal |
| `"$s" == pattern` | String matches glob pattern (no quotes on pattern) |
| `"$s" =~ regex` | String matches extended regex (bash only) |

### Integer tests

| Operator | True if |
|---|---|
| `$a -eq $b` | Equal |
| `$a -ne $b` | Not equal |
| `$a -lt $b` | Less than |
| `$a -le $b` | Less than or equal |
| `$a -gt $b` | Greater than |
| `$a -ge $b` | Greater than or equal |

In `(( ))` arithmetic context, use `==`, `!=`, `<`, `<=`, `>`, `>=` directly.

---

## Bash built-ins

| Built-in | Purpose |
|---|---|
| `echo` | Print text (avoid `-e`; not portable) |
| `printf` | Formatted output; portable and reliable |
| `read` | Read a line from stdin into a variable |
| `local` | Declare a function-scoped variable |
| `declare` | Declare variables with attributes (`-r`, `-i`, `-a`, `-A`) |
| `readonly` | Mark a variable as immutable |
| `export` | Make a variable available to child processes |
| `source` / `.` | Execute a script in the current shell context |
| `eval` | Execute a string as a shell command (use with extreme caution) |
| `mapfile` / `readarray` | Read lines from stdin into an array (bash 4+) |
| `typeset` | Alias for `declare` (also used in zsh) |
| `trap` | Register a command to run on a signal or exit |
| `wait` | Wait for background jobs to finish |
| `jobs` | List background jobs |
| `disown` | Remove a job from the shell's job table |
| `getopts` | Parse short option flags |
| `shift` | Shift positional parameters left by N |
| `set` | Set shell options or positional parameters |
| `unset` | Remove a variable or function |
| `pushd` / `popd` | Directory stack navigation |
| `command` | Bypass shell functions; run the external command directly |
| `type` | Show how a name is interpreted (function, built-in, file) |
| `compgen` | Generate completions (useful in scripts for listing commands) |

---

## Redirection

| Syntax | Meaning |
|---|---|
| `cmd > file` | Redirect stdout to file (overwrite) |
| `cmd >> file` | Redirect stdout to file (append) |
| `cmd < file` | Read stdin from file |
| `cmd 2> file` | Redirect stderr to file |
| `cmd 2>&1` | Redirect stderr to stdout |
| `cmd &> file` | Redirect both stdout and stderr to file (bash) |
| `cmd 2>/dev/null` | Discard stderr |
| `cmd > /dev/null 2>&1` | Discard all output (portable) |
| `cmd1 \| cmd2` | Pipe stdout of cmd1 to stdin of cmd2 |
| `cmd <<EOF ... EOF` | Here document - feed multi-line string as stdin |
| `cmd <<-EOF ... EOF` | Here document stripping leading tabs |
| `cmd <(other)` | Process substitution - treat output of `other` as a file |
| `cmd >(other)` | Process substitution - write to stdin of `other` via a file |

---

## Arrays

```bash
# Declare and populate
arr=("alpha" "beta" "gamma")
declare -a arr

# Access
echo "${arr[0]}"           # first element
echo "${arr[-1]}"          # last element (bash 4.3+)
echo "${arr[@]}"           # all elements (always quote)
echo "${#arr[@]}"          # number of elements
echo "${!arr[@]}"          # all indices

# Append
arr+=("delta")

# Slice: ${arr[@]:offset:length}
echo "${arr[@]:1:2}"       # elements 1 and 2

# Delete element
unset 'arr[1]'

# Iterate safely
for item in "${arr[@]}"; do
  echo "$item"
done

# Associative array (bash 4+)
declare -A map
map["key"]="value"
echo "${map["key"]}"
echo "${!map[@]}"          # all keys
```

---

## Arithmetic

```bash
# (( )) for integer arithmetic - returns exit code 0/1 for true/false
(( count++ ))
(( total = a + b * c ))
if (( count > 10 )); then echo "too many"; fi

# $(( )) for arithmetic substitution
result=$(( 2 ** 10 ))      # 1024
echo $(( RANDOM % 100 ))   # random 0-99

# bc for floating point
result=$(echo "scale=2; 22/7" | bc)
```

---

## Common patterns

```bash
# Default value for missing first argument
input="${1:-default.txt}"

# Require exactly one argument
[[ $# -eq 1 ]] || { echo "Usage: $0 <file>" >&2; exit 1; }

# Check if running as root
[[ $EUID -eq 0 ]] || { echo "Must run as root" >&2; exit 1; }

# Check if a command exists
command -v docker &>/dev/null || { echo "docker not found" >&2; exit 1; }

# Absolute path of the script's directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Silent background job
some_long_command &>/dev/null &

# Retry a command up to N times
retry() {
  local n="$1"; shift
  local delay="${1:-2}"; shift
  local i
  for (( i=1; i<=n; i++ )); do
    "$@" && return 0
    echo "Attempt $i/$n failed. Retrying in ${delay}s..." >&2
    sleep "$delay"
  done
  return 1
}
retry 3 2 curl -sf https://example.com
```
