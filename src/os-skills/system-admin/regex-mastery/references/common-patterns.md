<!-- Part of the regex-mastery AbsolutelySkilled skill. Load this file when
     the user asks for a ready-to-use regex for a common format or when
     building validation, extraction, or sanitization logic. -->

# Common Regex Patterns

Production-ready patterns for frequent use cases. All patterns are in JavaScript
regex literal syntax. Add the `u` flag when working with Unicode input. Test every
pattern against your specific input format before shipping.

---

## Email

```js
// Basic - catches most invalid formats, avoids RFC rabbit hole
const EMAIL = /^[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}$/

// With plus addressing and subdomain support (same pattern, illustrative)
EMAIL.test('user+tag@mail.example.co.uk') // true
EMAIL.test('not@valid')                   // false
```

---

## URL

```js
// HTTP/HTTPS with optional port, path, query, and fragment
const URL_HTTP = /^https?:\/\/(?:[\w\-]+\.)+[a-zA-Z]{2,}(?::\d{1,5})?(?:\/[^\s]*)?$/

// In Node.js / browser environments, prefer the URL constructor:
function isValidUrl(s) {
  try { new URL(s); return true } catch { return false }
}
```

---

## IPv4 Address

```js
// Exact 0-255 per octet
const IPV4 = /^(?:(?:25[0-5]|2[0-4]\d|[01]?\d\d?)\.){3}(?:25[0-5]|2[0-4]\d|[01]?\d\d?)$/

IPV4.test('192.168.1.255') // true
IPV4.test('256.1.1.1')     // false
IPV4.test('192.168.1')     // false
```

---

## IPv6 Address

```js
// Full 8-group form only - use a library (is-ip, ipaddr.js) for compressed forms
const IPV6_FULL = /^(?:[0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}$/

// Simplified pattern that also accepts :: compressed notation
const IPV6 = /^(([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|(([0-9a-fA-F]{1,4}:)*[0-9a-fA-F]{1,4})?::(([0-9a-fA-F]{1,4}:)*[0-9a-fA-F]{1,4})?)$/
```

---

## Phone Numbers

```js
// E.164 international format: +[country][number], 7-15 digits
const PHONE_E164 = /^\+[1-9]\d{6,14}$/

// North American (NANP) - flexible formatting
const PHONE_NANP = /^(\+1[-.\s]?)?(\(?\d{3}\)?[-.\s]?)?\d{3}[-.\s]?\d{4}$/

// Any digits-only string of 7-15 digits (for normalized storage)
const PHONE_DIGITS = /^\d{7,15}$/

PHONE_E164.test('+14155552671')    // true
PHONE_NANP.test('(415) 555-2671') // true
PHONE_NANP.test('415.555.2671')   // true
```

---

## Date Formats

```js
// ISO 8601 date: YYYY-MM-DD
const DATE_ISO = /^\d{4}-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])$/

// US format: MM/DD/YYYY
const DATE_US = /^(0[1-9]|1[0-2])\/(0[1-9]|[12]\d|3[01])\/\d{4}$/

// ISO 8601 datetime with optional timezone
const DATETIME_ISO = /^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?$/

// Note: regex can't validate Feb 30 or leap years - use Date.parse() for that
```

---

## UUID / GUID

```js
// UUID v4 (most common): 8-4-4-4-12 hex chars, third group starts with 4
const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i

// Any valid UUID (v1-v5)
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i

UUID_V4.test('550e8400-e29b-41d4-a716-446655440000') // true
UUID_V4.test('not-a-uuid')                           // false
```

---

## Semantic Version (semver)

```js
// Core semver: MAJOR.MINOR.PATCH with optional pre-release and build metadata
const SEMVER = /^(?<major>0|[1-9]\d*)\.(?<minor>0|[1-9]\d*)\.(?<patch>0|[1-9]\d*)(?:-(?<prerelease>(?:0|[1-9]\d*|\d*[a-zA-Z\-][0-9a-zA-Z\-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z\-][0-9a-zA-Z\-]*))*))?(?:\+(?<buildmetadata>[0-9a-zA-Z\-]+(?:\.[0-9a-zA-Z\-]+)*))?$/

const m = '1.2.3-alpha.1+build.42'.match(SEMVER)
m?.groups // { major: '1', minor: '2', patch: '3', prerelease: 'alpha.1', buildmetadata: 'build.42' }
```

---

## URL Slug

```js
// Lowercase alphanumeric with hyphens, no leading/trailing hyphens
const SLUG = /^[a-z0-9]+(?:-[a-z0-9]+)*$/

SLUG.test('my-blog-post-2026') // true
SLUG.test('-starts-with-dash') // false
SLUG.test('has--double-dash')  // false
```

---

## Password Strength Checks

```js
// At least 8 chars, one uppercase, one lowercase, one digit, one special char
const PASSWORD_STRONG = /^(?=.*[a-z])(?=.*[A-Z])(?=.*\d)(?=.*[!@#$%^&*()_+\-=\[\]{}|;':",.<>?]).{8,}$/

// Individual checks (more readable, easier to give specific error messages)
const HAS_LOWERCASE  = /[a-z]/
const HAS_UPPERCASE  = /[A-Z]/
const HAS_DIGIT      = /\d/
const HAS_SPECIAL    = /[!@#$%^&*()\-_=+\[\]{}|;':",.<>?\/\\]/
const MIN_LENGTH     = /^.{8,}$/
```

---

## Credit Card Numbers (Luhn check still required)

```js
// Visa: starts with 4, 13 or 16 digits
const CC_VISA = /^4[0-9]{12}(?:[0-9]{3})?$/

// Mastercard: starts with 51-55 or 2221-2720, 16 digits
const CC_MASTERCARD = /^(?:5[1-5]\d{2}|222[1-9]|22[3-9]\d|2[3-6]\d{2}|27[01]\d|2720)\d{12}$/

// American Express: starts with 34 or 37, 15 digits
const CC_AMEX = /^3[47][0-9]{13}$/

// Generic: 13-19 digits, optional spaces or dashes between groups
const CC_GENERIC = /^\d{4}[\s\-]?\d{4}[\s\-]?\d{4}[\s\-]?\d{1,7}$/

// Always run Luhn algorithm after regex match - regex only checks format
```

---

## Hex Color

```js
// 3 or 6 hex digit color codes (# prefix)
const HEX_COLOR = /^#(?:[0-9a-fA-F]{3}){1,2}$/

// Also matches 4 and 8 digit (with alpha channel, CSS Level 4)
const HEX_COLOR_ALPHA = /^#(?:[0-9a-fA-F]{3,4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/

HEX_COLOR.test('#fff')     // true
HEX_COLOR.test('#1a2b3c')  // true
HEX_COLOR.test('#xyz')     // false
```

---

## Postal / ZIP Codes

```js
// US ZIP code: 5 digits or ZIP+4
const ZIP_US = /^\d{5}(?:-\d{4})?$/

// UK postcode (simplified)
const POSTCODE_UK = /^[A-Z]{1,2}\d[A-Z\d]? ?\d[A-Z]{2}$/i

// Canadian postal code
const POSTAL_CA = /^[ABCEGHJ-NPRSTVXY]\d[A-Z] ?\d[A-Z]\d$/i

ZIP_US.test('90210')       // true
ZIP_US.test('90210-1234')  // true
ZIP_US.test('9021')        // false
```

---

## HTML/XML Tags (limited - use a parser for full documents)

```js
// Opening tag with optional attributes (simple cases only)
const HTML_OPEN_TAG = /<([a-zA-Z][a-zA-Z0-9]*)\b([^>]*)>/

// Self-closing tag
const HTML_SELF_CLOSING = /<([a-zA-Z][a-zA-Z0-9]*)\b([^>]*)\/>/

// Strip all HTML tags (for plain text extraction - not XSS-safe)
const STRIP_HTML = /<[^>]+>/g
'<b>Hello</b> <i>World</i>'.replace(STRIP_HTML, '') // 'Hello World'

// WARNING: Never use these for security-sensitive HTML sanitization.
// Use DOMPurify or a server-side sanitizer instead.
```

---

## Whitespace and Formatting

```js
// Trim leading/trailing whitespace (prefer String.prototype.trim() instead)
const TRIM = /^\s+|\s+$/g

// Collapse multiple internal whitespace to single space
const NORMALIZE_WHITESPACE = /\s+/g
'  hello   world  '.replace(NORMALIZE_WHITESPACE, ' ').trim() // 'hello world'

// Match blank lines
const BLANK_LINE = /^\s*$/m

// Match Windows-style CRLF line endings
const CRLF = /\r\n/g
```

---

## Numbers

```js
// Integer (positive or negative, no leading zeros)
const INTEGER = /^-?(?:0|[1-9]\d*)$/

// Decimal number (positive or negative, optional decimal part)
const DECIMAL = /^-?(?:0|[1-9]\d*)(?:\.\d+)?$/

// Currency amount (e.g. 1234.56 or 1,234.56)
const CURRENCY = /^\d{1,3}(?:,\d{3})*(?:\.\d{2})?$/

// Hexadecimal number (with optional 0x prefix)
const HEX_NUMBER = /^(?:0[xX])?[0-9a-fA-F]+$/

INTEGER.test('-42')   // true
INTEGER.test('007')   // false (leading zero)
DECIMAL.test('3.14')  // true
```

---

## Domain Names

```js
// FQDN (fully qualified domain name)
const DOMAIN = /^(?:[a-zA-Z0-9](?:[a-zA-Z0-9\-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]{2,}$/

// Subdomain extraction from a URL hostname
const SUBDOMAIN_REGEX = /^(?<subdomain>[\w\-]+)\.(?<domain>[\w\-]+\.[\w]{2,})$/

DOMAIN.test('www.example.com')    // true
DOMAIN.test('example.com')        // true
DOMAIN.test('-invalid.com')       // false (leading hyphen)
```

---

## Common Identifiers

```js
// GitHub username: alphanumeric and hyphens, 1-39 chars, no leading/trailing hyphen
const GITHUB_USERNAME = /^[a-zA-Z0-9](?:[a-zA-Z0-9\-]{0,37}[a-zA-Z0-9])?$/

// Twitter/X handle (without @)
const TWITTER_HANDLE = /^[a-zA-Z0-9_]{1,15}$/

// npm package name
const NPM_PACKAGE = /^(?:@[a-z0-9\-*~][a-z0-9\-*._~]*\/)?[a-z0-9\-~][a-z0-9\-._~]*$/

// Docker image tag
const DOCKER_TAG = /^[a-zA-Z0-9_\-\.]{1,128}$/
```
