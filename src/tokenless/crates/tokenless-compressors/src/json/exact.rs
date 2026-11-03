//! Exact JSON documents for the JSON compression domain.
//!
//! `serde_json::Value` stores every number as `i64`, `u64`, or `f64`, so
//! round-tripping a document through it rewrites numeric literals: `1e-400`
//! becomes `0.0`, `18446744073709551617` becomes `1.8446744073709552e19`, and
//! `-0` becomes `-0.0`. The compressor therefore parses into [`ExactValue`],
//! which keeps each number as the validated source token it was scanned from,
//! and serializes through [`write`], which emits that token verbatim.
//!
//! Two properties make this safe where a global `serde_json` feature would not
//! be:
//!
//! * Numbers are retained as data owned by this crate. No `raw_value` or
//!   `arbitrary_precision` feature is enabled, so Cargo feature unification
//!   cannot change how the rest of the workspace parses JSON, and an ordinary
//!   object such as `{"$serde_json::private::RawValue":"123"}` stays an object.
//! * String leaves are still parsed by `serde_json`, so escapes, surrogate
//!   pairs, and control-byte rejection have one source of truth.
//!
//! A literal is retained only when `serde_json` can read it back
//! ([`retainable`]): Stash payloads and compressor output must stay parseable
//! by the rest of the toolchain.

use std::fmt::Write as _;

use indexmap::IndexMap;
use serde_json::Value;

/// Deepest container nesting this scanner accepts.
///
/// `serde_json` rejects the 128th nested container, so a document it accepts
/// never reaches this bound and a document it rejects never recurses deeper.
/// Bounding the scan here is what keeps adversarial nesting from overflowing
/// the stack before `serde_json` gets to say no.
const MAX_CONTAINER_DEPTH: usize = 127;

/// Object member storage, ordered exactly like `serde_json::Map`.
///
/// `toon-format` turns on `serde_json/preserve_order`, so Cargo feature
/// unification compiles `serde_json` with an `IndexMap`-backed `Map` for every
/// crate in this workspace: compact serialization has always emitted object
/// members in source order. The exact tree uses the same map, so retaining
/// numeric literals cannot quietly reorder objects, and a repeated key keeps its
/// first position with its last value exactly as `serde_json` does.
pub(crate) type ExactMap<'a> = IndexMap<String, ExactValue<'a>>;

/// A JSON document whose numeric literals are retained verbatim.
///
/// Derived equality is spelling-sensitive: `1.2500` and `1.25` are different
/// documents even though their decimal values match. Compare numbers by value
/// with [`decimal_value`] when spelling must not matter.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ExactValue<'a> {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number token copied from the source document.
    Number(&'a str),
    /// A decoded string.
    String(String),
    /// Array items in source order.
    Array(Vec<ExactValue<'a>>),
    /// Object members keyed like `serde_json::Map`.
    Object(ExactMap<'a>),
}

impl<'a> ExactValue<'a> {
    /// Type name used by depth-truncation markers.
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "bool",
            Self::Number(_) => "number",
            Self::String(_) => "string",
            Self::Array(_) => "array",
            Self::Object(_) => "object",
        }
    }

    /// String payload, when this is a string.
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    /// Object members, when this is an object.
    pub(crate) fn as_object(&self) -> Option<&ExactMap<'a>> {
        match self {
            Self::Object(value) => Some(value),
            _ => None,
        }
    }

    /// Whether this is an empty string or container.
    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Self::String(value) => value.is_empty(),
            Self::Array(value) => value.is_empty(),
            Self::Object(value) => value.is_empty(),
            _ => false,
        }
    }

    /// Whether this is empty or null, for record importance signals.
    pub(crate) fn is_empty_or_null(&self) -> bool {
        matches!(self, Self::Null) || self.is_empty()
    }

    /// Whether this is an object or an array.
    pub(crate) fn is_container(&self) -> bool {
        matches!(self, Self::Object(_) | Self::Array(_))
    }

    /// Whether this is an object.
    pub(crate) fn is_object(&self) -> bool {
        matches!(self, Self::Object(_))
    }
}

/// Rejection of a document by [`parse`].
///
/// Carries no message: the caller asks `serde_json` for the authoritative
/// diagnostic, because this scanner accepts exactly the documents `serde_json`
/// accepts.
#[derive(Debug)]
pub(crate) struct ScanRejected;

/// Parses one JSON document, retaining every number token verbatim.
///
/// # Errors
///
/// Returns [`ScanRejected`] when the source is not a document this scanner
/// accepts. The caller re-derives the `serde_json` diagnostic.
pub(crate) fn parse(source: &str) -> Result<ExactValue<'_>, ScanRejected> {
    Scanner::new(source).scan()
}

struct Scanner<'a> {
    text: &'a str,
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Scanner<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            position: 0,
        }
    }

    fn scan(mut self) -> Result<ExactValue<'a>, ScanRejected> {
        self.skip_whitespace();
        let value = self.value(0)?;
        self.skip_whitespace();
        if self.position == self.bytes.len() {
            Ok(value)
        } else {
            Err(ScanRejected)
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.position += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<ExactValue<'a>, ScanRejected> {
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(ExactValue::String(self.string()?)),
            Some(b't') => self.keyword(b"true", ExactValue::Bool(true)),
            Some(b'f') => self.keyword(b"false", ExactValue::Bool(false)),
            Some(b'n') => self.keyword(b"null", ExactValue::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(ScanRejected),
        }
    }

    fn keyword(
        &mut self,
        word: &[u8],
        value: ExactValue<'a>,
    ) -> Result<ExactValue<'a>, ScanRejected> {
        let end = self.position.checked_add(word.len()).ok_or(ScanRejected)?;
        if self.bytes.get(self.position..end) == Some(word) {
            self.position = end;
            Ok(value)
        } else {
            Err(ScanRejected)
        }
    }

    fn object(&mut self, depth: usize) -> Result<ExactValue<'a>, ScanRejected> {
        if depth == MAX_CONTAINER_DEPTH {
            return Err(ScanRejected);
        }
        self.position += 1;
        let mut members = ExactMap::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.position += 1;
            return Ok(ExactValue::Object(members));
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            self.skip_whitespace();
            if self.peek() != Some(b':') {
                return Err(ScanRejected);
            }
            self.position += 1;
            self.skip_whitespace();
            let value = self.value(depth + 1)?;
            // A repeated key keeps its last value, matching `serde_json`.
            members.insert(key, value);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b'}') => {
                    self.position += 1;
                    return Ok(ExactValue::Object(members));
                }
                _ => return Err(ScanRejected),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<ExactValue<'a>, ScanRejected> {
        if depth == MAX_CONTAINER_DEPTH {
            return Err(ScanRejected);
        }
        self.position += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.position += 1;
            return Ok(ExactValue::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b']') => {
                    self.position += 1;
                    return Ok(ExactValue::Array(items));
                }
                _ => return Err(ScanRejected),
            }
        }
    }

    fn string(&mut self) -> Result<String, ScanRejected> {
        if self.peek() != Some(b'"') {
            return Err(ScanRejected);
        }
        let start = self.position;
        self.position += 1;
        let mut needs_decoding = false;
        loop {
            match self.peek() {
                Some(b'\\') => {
                    needs_decoding = true;
                    self.position += 1;
                    if self.peek().is_none() {
                        return Err(ScanRejected);
                    }
                    self.position += 1;
                }
                Some(b'"') => {
                    self.position += 1;
                    break;
                }
                Some(byte) => {
                    // Raw control bytes are rejected inside strings; hand those
                    // to `serde_json` rather than restating the rule here.
                    needs_decoding |= byte < 0x20;
                    self.position += 1;
                }
                None => return Err(ScanRejected),
            }
        }
        // `serde_json` owns string semantics: escapes, surrogate pairs, and the
        // rejection of raw control bytes. Delegating every escaped leaf keeps
        // this scanner from becoming a second, subtly different source of
        // truth. An unescaped payload needs no decoding, and copying it
        // directly is what keeps string-heavy documents at one pass; UTF-8
        // continuation bytes are all above ASCII, so the byte scan above only
        // ever stops on a character boundary.
        let raw = self.text.get(start..self.position).ok_or(ScanRejected)?;
        if needs_decoding {
            return serde_json::from_str::<String>(raw).map_err(|_| ScanRejected);
        }
        Ok(raw[1..raw.len() - 1].to_owned())
    }

    fn number(&mut self) -> Result<ExactValue<'a>, ScanRejected> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        match self.peek() {
            Some(b'0') => self.position += 1,
            Some(b'1'..=b'9') => {
                self.position += 1;
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.position += 1;
                }
            }
            _ => return Err(ScanRejected),
        }
        if self.peek() == Some(b'.') {
            self.position += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(ScanRejected);
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.position += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(ScanRejected);
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.position += 1;
            }
        }
        let literal = self.text.get(start..self.position).ok_or(ScanRejected)?;
        retainable(literal)
            .then_some(ExactValue::Number(literal))
            .ok_or(ScanRejected)
    }
}

/// Whether a validated number token can be retained without breaking readers.
///
/// `serde_json` rejects a number that leaves the finite `f64` range, so a
/// literal it cannot read must not reach compressor output or a Stash payload:
/// retrieval parses those with `serde_json`. The grammar is already enforced by
/// [`Scanner::number`]; this is the range half of the same acceptance rule.
fn retainable(literal: &str) -> bool {
    literal.parse::<u64>().is_ok()
        || literal.parse::<i64>().is_ok()
        || literal.parse::<f64>().is_ok_and(f64::is_finite)
}

/// Approximate value of a retained literal, for selection statistics only.
///
/// The result is never written to output. `None` means the literal has no
/// finite `f64` approximation and therefore cannot join mean/variance
/// statistics at all; callers decide what that costs, and must not treat the
/// number as ordinary.
pub(crate) fn approximate(literal: &str) -> Option<f64> {
    literal
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

/// Exact decimal value of a JSON number token.
///
/// Compared without floating-point conversion, so equivalent spellings (`1e3`,
/// `1000.0`) share a sign, zero-stripped coefficient, and exponent, while
/// values that differ beyond `f64` precision stay distinct. Negative zero keeps
/// its sign and is therefore never equal to `0`.
pub(crate) fn decimal_value(literal: &str) -> Option<(bool, String, i64)> {
    let (negative, literal) = match literal.strip_prefix('-') {
        Some(literal) => (true, literal),
        None => (false, literal),
    };
    let (mantissa, exponent) = match literal.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
        None => (literal, 0),
    };
    let fractional = mantissa
        .split_once('.')
        .map_or(0, |(_, fractional)| fractional.len());
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some((negative, String::new(), 0));
    }
    let coefficient = digits.trim_end_matches('0');
    let trailing = i64::try_from(digits.len() - coefficient.len()).ok()?;
    let exponent = exponent
        .checked_sub(i64::try_from(fractional).ok()?)?
        .checked_add(trailing)?;
    Some((negative, coefficient.to_owned(), exponent))
}

/// Writes `value` as compact JSON, emitting every number token verbatim.
///
/// String escaping matches `serde_json`'s serializer, so a document whose
/// numbers `serde_json` already reproduces exactly serializes byte for byte the
/// same way it did before numbers were retained.
pub(crate) fn write(value: &ExactValue<'_>, output: &mut String) {
    write_value(value, output, false);
}

/// Writes `value` as compact JSON into a new string.
pub(crate) fn write_to_string(value: &ExactValue<'_>) -> String {
    let mut output = String::new();
    write(value, &mut output);
    output
}

/// Writes a slice of items as a compact JSON array into a new string.
///
/// Used for Stash payloads, which must carry the same exact numbers as the
/// document they came from.
pub(crate) fn write_array_to_string(items: &[ExactValue<'_>]) -> String {
    let mut output = String::with_capacity(items.len() * 32 + 2);
    output.push('[');
    for (offset, item) in items.iter().enumerate() {
        if offset > 0 {
            output.push(',');
        }
        write_value(item, &mut output, false);
    }
    output.push(']');
    output
}

/// Writes `value` as a record-deduplication key.
///
/// Structurally identical to [`write`] except that numbers are normalized to
/// their exact decimal value, so two records differing only in numeric spelling
/// (`{"v":1.2500}` and `{"v":1.25}`) are recognized as duplicates and only the
/// first is kept as a sample. The retained record still carries its own
/// spelling; normalization exists only inside the key.
pub(crate) fn write_dedup_key(value: &ExactValue<'_>, output: &mut String) {
    write_value(value, output, true);
}

fn write_value(value: &ExactValue<'_>, output: &mut String, canonical_numbers: bool) {
    match value {
        ExactValue::Null => output.push_str("null"),
        ExactValue::Bool(true) => output.push_str("true"),
        ExactValue::Bool(false) => output.push_str("false"),
        ExactValue::Number(literal) => {
            if canonical_numbers {
                write_canonical_number(literal, output);
            } else {
                output.push_str(literal);
            }
        }
        ExactValue::String(value) => write_string(value, output),
        ExactValue::Array(items) => {
            output.push('[');
            for (offset, item) in items.iter().enumerate() {
                if offset > 0 {
                    output.push(',');
                }
                write_value(item, output, canonical_numbers);
            }
            output.push(']');
        }
        ExactValue::Object(members) => {
            output.push('{');
            for (offset, (key, member)) in members.iter().enumerate() {
                if offset > 0 {
                    output.push(',');
                }
                write_string(key, output);
                output.push(':');
                write_value(member, output, canonical_numbers);
            }
            output.push('}');
        }
    }
}

fn write_canonical_number(literal: &str, output: &mut String) {
    match decimal_value(literal) {
        Some((negative, coefficient, exponent)) => {
            if negative {
                output.push('-');
            }
            if coefficient.is_empty() {
                output.push('0');
            } else {
                output.push_str(&coefficient);
            }
            output.push('e');
            let _ = write!(output, "{exponent}");
        }
        // Every retained literal has a decimal value. Should one ever not,
        // fall back to its spelling rather than collapse two distinct numbers
        // into a single deduplication key.
        None => output.push_str(literal),
    }
}

/// Bytes that cannot be copied into a JSON string verbatim.
const NEEDS_ESCAPE: [bool; 256] = {
    let mut table = [false; 256];
    table[b'"' as usize] = true;
    table[b'\\' as usize] = true;
    let mut control = 0;
    while control < 0x20 {
        table[control] = true;
        control += 1;
    }
    table
};

/// Escapes a string exactly the way `serde_json`'s serializer does: `"` and `\`
/// short-escaped, the five named control characters short-escaped, remaining
/// control characters as lowercase `\u00xx`, and everything else raw UTF-8.
/// Escaped bytes are rare, so runs between them are copied in one piece.
fn write_string(value: &str, output: &mut String) {
    output.push('"');
    let bytes = value.as_bytes();
    let mut flushed = 0;
    while let Some(offset) = bytes[flushed..]
        .iter()
        .position(|byte| NEEDS_ESCAPE[*byte as usize])
    {
        let index = flushed + offset;
        output.push_str(&value[flushed..index]);
        match bytes[index] {
            b'"' => output.push_str("\\\""),
            b'\\' => output.push_str("\\\\"),
            0x08 => output.push_str("\\b"),
            b'\t' => output.push_str("\\t"),
            b'\n' => output.push_str("\\n"),
            0x0c => output.push_str("\\f"),
            b'\r' => output.push_str("\\r"),
            byte => {
                let _ = write!(output, "\\u{byte:04x}");
            }
        }
        flushed = index + 1;
    }
    output.push_str(&value[flushed..]);
    output.push('"');
}

/// Approximates a document as `serde_json::Value` for TOON encoding.
///
/// Numbers lose precision here by construction. The result only drives
/// `toon_format`; it must never reach compressor output or a Stash payload.
/// [`matches_exact`] decides whether an encoded candidate may replace the exact
/// document it was built from.
pub(crate) fn to_serde_value(value: &ExactValue<'_>) -> Option<Value> {
    Some(match value {
        ExactValue::Null => Value::Null,
        ExactValue::Bool(flag) => Value::Bool(*flag),
        ExactValue::Number(literal) => Value::Number(serde_json::from_str(literal).ok()?),
        ExactValue::String(text) => Value::String(text.clone()),
        ExactValue::Array(items) => Value::Array(
            items
                .iter()
                .map(to_serde_value)
                .collect::<Option<Vec<_>>>()?,
        ),
        ExactValue::Object(members) => {
            let mut object = serde_json::Map::new();
            for (key, member) in members {
                object.insert(key.clone(), to_serde_value(member)?);
            }
            Value::Object(object)
        }
    })
}

/// Whether a decoded candidate retains the exact document it was encoded from.
///
/// Structure, string content, and numeric type must all match. Numbers are
/// compared by exact decimal value rather than by `f64`, because two
/// already-rounded `serde_json` trees agree on `1.234567890123456789` and
/// `1.2345678901234567` while the literals do not.
///
/// Spelling policy: an equivalent spelling may be normalized (`1.2500` may come
/// back as `1.25`, `1e3` may come back as `1000.0`) as long as the decimal
/// value and the integer/float class are unchanged. Negative zero must keep its
/// sign, so a candidate that reads `-0` back as `0` is rejected.
pub(crate) fn matches_exact(candidate: &Value, expected: &ExactValue<'_>) -> bool {
    match (candidate, expected) {
        (Value::Null, ExactValue::Null) => true,
        (Value::Bool(candidate), ExactValue::Bool(expected)) => candidate == expected,
        (Value::Number(candidate), ExactValue::Number(expected)) => {
            number_matches(candidate, expected)
        }
        (Value::String(candidate), ExactValue::String(expected)) => candidate == expected,
        (Value::Array(candidate), ExactValue::Array(expected)) => {
            candidate.len() == expected.len()
                && candidate
                    .iter()
                    .zip(expected)
                    .all(|(item, expected)| matches_exact(item, expected))
        }
        (Value::Object(candidate), ExactValue::Object(expected)) => {
            candidate.len() == expected.len()
                && expected.iter().all(|(key, member)| {
                    candidate
                        .get(key)
                        .is_some_and(|value| matches_exact(value, member))
                })
        }
        _ => false,
    }
}

fn number_matches(candidate: &serde_json::Number, expected: &str) -> bool {
    // The literal is read the way `serde_json` reads it, which is also the way
    // it was handed to the encoder. That fixes the expected integer/float class
    // without restating `serde_json`'s rules here, negative zero included.
    let Ok(source) = serde_json::from_str::<serde_json::Number>(expected) else {
        return false;
    };
    let Some(candidate_value) = decimal_value(&candidate.to_string()) else {
        return false;
    };
    let Some(expected_value) = decimal_value(expected) else {
        return false;
    };
    is_integer(candidate) == is_integer(&source) && candidate_value == expected_value
}

fn is_integer(number: &serde_json::Number) -> bool {
    number.is_i64() || number.is_u64()
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("../tests/json_exact_tests.rs");
}
