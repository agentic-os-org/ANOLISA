---
name: regex-mastery
version: 0.1.0
description: Use this skill when writing regular expressions, debugging pattern matching,optimizing regex performance, or implementing text validation. Triggers on regex, regular expressions, pattern matching, lookahead, lookbehind, named groups, capture groups, backreferences, and any task requiring text pattern matching.
layer: application
lifecycle: production
category: devtools
tags: [regex, patterns, text-processing, validation, matching]
dependencies: [shell-scripting, vim-neovim, debugging-tools, cli-design]
platforms:
  - claude-code
  - gemini-cli
  - openai-codex
license: MIT
maintainers:
  - github: maddhruv
---
When this skill is activated, always start your first response with the 🧢 emoji.

# Regex Mastery

Regular expressions are a compact language for describing text patterns, built into
virtually every programming language and text processing tool. They power input
validation, log parsing, data extraction, search-and-replace, and tokenization.
Used well, a single regex can replace dozens of lines of string manipulation code.
Used poorly, they become unreadable traps and can grind a server to a halt via
catastrophic backtracking.

---

## When to use this skill

Trigger this skill when the user:

- Asks to write or explain a regular expression
- Wants to validate input format (email, URL, phone number, date, credit card)
- Needs to extract data from structured or semi-structured text (logs, CSV, HTML)
- Uses regex terminology: lookahead, lookbehind, named group, capture group, backreference
- Wants to debug a pattern that isn't matching as expected
- Asks about regex flags (`i`, `g`, `m`, `s`, `u`, `x`)
- Needs to replace text using capture groups or back-references

Do NOT trigger this skill for:

- Full HTML/XML parsing (use a proper parser like DOMParser or BeautifulSoup instead)
- Complex natural language processing where ML models are a better fit

---

## Key principles

1. **Readability over cleverness** - A regex that nobody can maintain is worse than
   a slightly longer explicit approach. Break complex patterns into commented steps
   or use the verbose (`x`) flag where supported. A named group costs nothing but
   pays dividends every time someone reads the pattern.
2. **Use named capture groups** - `(?<year>\d{4})` is self-documenting and immune
   to positional breakage when the pattern changes. Always prefer named groups over
   numbered groups for any regex that will be read or maintained by humans.
3. **Test edge cases relentlessly** - Empty string, Unicode characters, very long
   input, malformed-but-close input (e.g., `foo@bar` for email), and adversarial
   input designed to trigger backtracking. A regex that passes your happy path but
   fails on a Unicode em-dash will cause production incidents.
4. **Avoid catastrophic backtracking** - Nested quantifiers (`(a+)+`) and overlapping
   alternatives (`(a|ab)+`) cause exponential backtracking on non-matching input.
   Use atomic groups or possessive quantifiers where available, or restructure
   alternation so choices are mutually exclusive.
5. **Use the right tool** - Regex is not always the answer. Parsing emails to RFC 5321
   compliance requires a full parser. Parsing JSON, HTML, or XML requires a DOM/SAX
   parser. If a regex exceeds ~80 characters or requires >2 levels of nesting, pause
   and ask whether a small state machine or parser would be clearer.

---

## Core concepts

**Greedy vs lazy quantifiers** - `*`, `+`, `?`, and `{n,m}` are greedy by default:
they match as much as possible while still allowing the overall pattern to succeed.
Add `?` to make them lazy (`*?`, `+?`): they match as little as possible. In
`<.+>` matching `<b>text</b>`, greedy gives the whole string; lazy `<.+?>` gives
just `<b>`.

**Backtracking engine** - Most regex engines (NFA-based: JS, Python, Java, .NET,
PCRE) work by trying a path and backing up when it fails. The cost of a failed match
can be exponential if quantifiers are nested and the pattern allows too many
overlapping interpretations. POSIX (DFA-based) engines don't backtrack but lack
lookaheads and backreferences.

**Character classes** - `[abc]` matches any one of a, b, c. `[^abc]` is the negation.
Shorthand classes: `\d` (digit), `\w` (word char), `\s` (whitespace), `\D`, `\W`,
`\S` (their negations). The `.` metacharacter matches any character except newline
(unless the `s`/dotall flag is set). Always prefer `\d` over `[0-9]` for clarity,
and `[^\n]` over `.` when you mean "not newline".

**Anchors** - `^` and `$` match start/end of string (or line with the `m` flag).
`\b` is a word boundary (zero-width). `\A`, `\Z` are absolute start/end of string
in Python (unaffected by multiline mode). Use anchors aggressively - an unanchored
pattern can match anywhere in the string, which is often not what you want.

**Groups and alternation** - `(abc)` is a capturing group; `(?:abc)` is
non-capturing (slightly faster, doesn't pollute `$1`/match.groups). Named groups:
`(?<name>abc)` in JS/Python/PCRE. Alternation `a|b` is left-to-right and short-circuits

- put the most common or most specific branch first. Backreferences `\1` or `\k<name>`
  match the same text captured by a group.

---

## Common tasks

### Validate an email address (basic)

A practical email regex that catches most invalid formats without attempting full
RFC compliance (which would require a 6553-character pattern).

```js
const emailRegex = /^[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}$/

function isValidEmail(email) {
  return emailRegex.test(email.trim())
}

// Examples
isValidEmail('user@example.com')     // true
isValidEmail('user+tag@sub.co.uk')   // true
isValidEmail('notanemail')           // false
isValidEmail('@nodomain.com')        // false
```

> Never use regex alone as the authoritative email validator in security-sensitive
> code. Always send a confirmation link. The only true validator is delivery.

### Validate a URL

```js
const urlRegex = /^https?:\/\/(?:[\w\-]+\.)+[a-zA-Z]{2,}(?::\d{1,5})?(?:\/[^\s]*)?$/

function isValidUrl(url) {
  try {
    new URL(url)   // prefer the URL constructor in JS environments
    return true
  } catch {
    return false
  }
}
```

> Prefer the native `URL` constructor in JS/Node.js over regex for URL validation.
> It handles edge cases like IPv6, IDN hostnames, and percent-encoded paths correctly.

### Validate a phone number (E.164 format)

```js
// E.164: +[country code][subscriber number], 7-15 digits total
const e164Regex = /^\+[1-9]\d{6,14}$/

// North American (NANP) with flexible formatting
const nanpRegex = /^(\+1[-.\s]?)?(\(?\d{3}\)?[-.\s]?)?\d{3}[-.\s]?\d{4}$/

e164Regex.test('+14155552671')     // true
e164Regex.test('4155552671')       // false (no + prefix)
nanpRegex.test('(415) 555-2671')   // true
nanpRegex.test('415.555.2671')     // true
```

### Extract data with named capture groups

Named groups make extraction code self-documenting and resilient to group reordering.

```js
const logLineRegex = /^\[(?<timestamp>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})\] (?<level>INFO|WARN|ERROR) (?<message>.+)$/m

const line = '[2026-03-14T09:41:00] ERROR Database connection refused'
const match = line.match(logLineRegex)

if (match) {
  const { timestamp, level, message } = match.groups
  console.log(timestamp) // '2026-03-14T09:41:00'
  console.log(level)     // 'ERROR'
  console.log(message)   // 'Database connection refused'
}
```

### Use lookahead and lookbehind

Lookarounds are zero-width assertions - they check context without consuming characters.

```js
// Positive lookahead: password must contain a digit
const hasDigit = /(?=.*\d)/
// Negative lookahead: word not followed by "(deprecated)"
const notDeprecated = /\bfoo\b(?!\s*\(deprecated\))/

// Positive lookbehind: price value preceded by $
const priceRegex = /(?<=\$)\d+(?:\.\d{2})?/g
'Total: $49.99 and $5.00'.match(priceRegex) // ['49.99', '5.00']

// Negative lookbehind: "port" not preceded by "trans"
const portNotTransport = /(?<!trans)port/gi
```

> Lookbehind (`(?<=...)` and `(?<!...)`) is supported in V8 (Node.js/Chrome),
> .NET, and Python 3.1+, but NOT in Safari < 16.4 or older PCRE. Check target
> environment before using.

### Replace with capture groups

Use `$1` / `$<name>` in the replacement string to insert captured text.

```js
// Reformat date from MM/DD/YYYY to YYYY-MM-DD
const date = '03/14/2026'
const reformatted = date.replace(
  /^(?<month>\d{2})\/(?<day>\d{2})\/(?<year>\d{4})$/,
  '$<year>-$<month>-$<day>'
)
// '2026-03-14'

// Wrap all @mentions in an anchor tag
const text = 'Hello @alice and @bob'
const linked = text.replace(/@(\w+)/g, '<a href="/user/$1">@$1</a>')
// 'Hello <a href="/user/alice">@alice</a> and <a href="/user/bob">@bob</a>'
```

### Avoid catastrophic backtracking

The classic trap: alternation inside a repeated group where alternatives overlap.

```js
// DANGEROUS - exponential time on non-matching input
const bad = /^(a+)+$/
bad.test('aaaaaaaaaaaaaaaaaaaaaaab') // hangs

// SAFE - remove the nested quantifier
const good = /^a+$/
good.test('aaaaaaaaaaaaaaaaaaaaaaab') // instant false

// SAFE alternative using atomic-group emulation via possessive quantifier (PCRE)
// In JS, restructure so the branches are mutually exclusive:
const safe = /^(?:a|b)+$/  // fine because a and b can't both match the same char
```

> Any time you write `(x+)+`, `(x|y)+` where x and y can match the same char, or
> deeply nested quantifiers, stop and test with a 30-character non-matching string.
> If it hangs, restructure.

### Parse structured text (log lines)

Use `exec` in a loop with the `g` flag to iterate over all matches.

```js
const accessLogRegex = /^(?<ip>\d{1,3}(?:\.\d{1,3}){3}) - - \[(?<time>[^\]]+)\] "(?<method>GET|POST|PUT|DELETE|PATCH) (?<path>[^ ]+) HTTP\/\d\.\d" (?<status>\d{3}) (?<bytes>\d+)/gm

const log = `192.168.1.1 - - [14/Mar/2026:09:41:00 +0000] "GET /api/users HTTP/1.1" 200 1234
10.0.0.2 - - [14/Mar/2026:09:41:01 +0000] "POST /api/login HTTP/1.1" 401 89`

for (const match of log.matchAll(accessLogRegex)) {
  const { ip, method, path, status } = match.groups
  console.log(`${ip} ${method} ${path} -> ${status}`)
}
```

### Use regex with Unicode

JavaScript requires the `u` flag for correct Unicode handling. The `v` flag (ES2024)
adds set notation and string properties.

```js
// WITHOUT u flag - counts UTF-16 code units, breaks on emoji
/^.{3}$/.test('a😀b')  // false (emoji is 2 code units, pattern sees 4 chars)

// WITH u flag - counts Unicode code points correctly
/^.{3}$/u.test('a😀b') // true

// Match any Unicode letter (requires u or v flag)
const wordChars = /[\p{L}\p{N}_]+/u

// Match emoji
const emoji = /\p{Emoji_Presentation}/gu

// Named Unicode blocks
const cyrillicWord = /^\p{Script=Cyrillic}+$/u
cyrillicWord.test('Привет') // true
```

---

## Anti-patterns / common mistakes

| Mistake                                  | Why it's wrong                                                                                        | What to do instead                                                        |
| ---------------------------------------- | ----------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| Unanchored validation pattern            | `/\d+/` matches the digits inside `"abc123def"`, so `test()` returns `true` for invalid input | Always add `^` and `$` anchors for validation patterns                |
| Numbered groups in maintained code       | `match[3]` breaks silently when a group is added                                                    | Use named groups:`match.groups.year`                                    |
| Using `.` to mean "any character"      | `.` matches everything except newline; bugs appear on multiline input                               | Use `[\s\S]` or set the `s` (dotAll) flag when newlines should match  |
| Greedy `.*` in the middle of a pattern | `"<b>one</b><b>two</b>".match(/<b>.*<\/b>/)` returns the whole string                               | Use lazy `.*?` or a negated class `[^<]*` when bounded by a delimiter |
| Rebuilding the same regex in a loop      | `new RegExp(pattern)` inside a `for` loop re-compiles on every iteration                          | Hoist the regex to a constant outside the loop                            |
| Parsing HTML/XML with regex              | Fails on nested tags, self-closing tags, CDATA, and valid edge cases                                  | Use DOMParser, jsdom, BeautifulSoup, or an XML library                    |

---

## References

For ready-to-use patterns across common domains, read:

- `references/common-patterns.md` - 20+ production-ready regex patterns for email,
  URL, phone, date, IP, UUID, passwords, slugs, semver, credit cards, and more

Only load the references file when you need a specific pattern - it is long and
will consume context.

---

## Related skills

> When this skill is activated, check if the following companion skills are installed.
> For any that are missing, mention them to the user and offer to install before proceeding
> with the task. Example: "I notice you don't have [skill] installed yet - it pairs well
> with this skill. Want me to install it?"

- [shell-scripting](https://github.com/AbsolutelySkilled/AbsolutelySkilled/tree/main/skills/shell-scripting) - Writing bash or zsh scripts, parsing arguments, handling errors, or automating CLI workflows.
- [vim-neovim](https://github.com/AbsolutelySkilled/AbsolutelySkilled/tree/main/skills/vim-neovim) - Configuring Neovim, writing Lua plugins, setting up keybindings, or optimizing the Vim editing workflow.
- [debugging-tools](https://github.com/AbsolutelySkilled/AbsolutelySkilled/tree/main/skills/debugging-tools) - Debugging applications using Chrome DevTools, lldb, strace, network tools, or memory profilers.
- [cli-design](https://github.com/AbsolutelySkilled/AbsolutelySkilled/tree/main/skills/cli-design) - Building command-line interfaces, designing CLI argument parsers, writing help text,...

Install a companion: `npx skills add AbsolutelySkilled/AbsolutelySkilled --skill <name>`
