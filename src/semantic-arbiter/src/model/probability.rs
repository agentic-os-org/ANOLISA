//! Exact decimal probabilities retain provider tokens without rounding into permission.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;

use super::ModelError;
use crate::protocol::MAX_MESSAGE_BYTES;

const PPM: u32 = 1_000_000;

/// Independent P(yes), retaining the original JSON number spelling in serialization and equality.
#[derive(Clone, Debug, Serialize)]
pub struct NoulProbability {
    raw: Box<RawValue>,
    #[serde(skip)]
    approximate: f64,
    yes_ppm: u32,
    no_ppm: u32,
}

impl NoulProbability {
    /// Normalize an injected finite float in `[0, 1]`; use `from_json_number` for lossless provider input.
    pub fn new(raw: f64) -> Result<Self, ModelError> {
        if !raw.is_finite() || !(0.0..=1.0).contains(&raw) {
            return Err(ModelError::InvalidResponse);
        }
        let token = serde_json::to_string(&raw).map_err(|_| ModelError::InvalidResponse)?;
        Self::from_json_number(&token)
    }

    /// Accept a bounded exact JSON number in `[0, 1]`, including exponents and negative zero, without surrounding whitespace.
    pub fn from_json_number(token: &str) -> Result<Self, ModelError> {
        let (yes_ppm, no_ppm) = normalize(token)?;
        // This approximation is for compatibility only, never bounds or permission.
        let approximate = token
            .parse::<f64>()
            .map_err(|_| ModelError::InvalidResponse)?;
        if !approximate.is_finite() {
            return Err(ModelError::InvalidResponse);
        }
        let raw =
            RawValue::from_string(token.to_owned()).map_err(|_| ModelError::InvalidResponse)?;
        Ok(Self {
            raw,
            approximate,
            yes_ppm,
            no_ppm,
        })
    }

    /// Lossy float approximation; do not use for validation, thresholds, or round trips.
    pub fn raw(&self) -> f64 {
        self.approximate
    }

    /// Original validated JSON number token, suitable for lossless revalidation.
    pub fn raw_json_number(&self) -> &str {
        self.raw.get()
    }

    /// Exact floor of one million times P(yes).
    pub fn yes_ppm(&self) -> u32 {
        self.yes_ppm
    }

    /// Exact floor of one million times (1 - P(yes)).
    pub fn no_ppm(&self) -> u32 {
        self.no_ppm
    }

    /// Compare exact yes support against an integer-millionths threshold.
    pub fn supports_yes(&self, threshold: u32) -> bool {
        // floor(P * PPM) >= t is equivalent to P >= t / PPM for integer t.
        self.yes_ppm >= threshold
    }

    /// Compare exact complementary support against an integer-millionths threshold.
    pub fn supports_no(&self, threshold: u32) -> bool {
        self.no_ppm >= threshold
    }
}

impl PartialEq for NoulProbability {
    fn eq(&self, other: &Self) -> bool {
        self.raw.get() == other.raw.get()
            && self.yes_ppm == other.yes_ppm
            && self.no_ppm == other.no_ppm
    }
}

impl<'de> Deserialize<'de> for NoulProbability {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Normalized {
            raw: Box<RawValue>,
            yes_ppm: u32,
            no_ppm: u32,
        }
        let fields = Normalized::deserialize(deserializer)?;
        let value = Self::from_json_number(fields.raw.get()).map_err(serde::de::Error::custom)?;
        if value.yes_ppm != fields.yes_ppm || value.no_ppm != fields.no_ppm {
            return Err(serde::de::Error::custom("invalid normalized probability"));
        }
        Ok(value)
    }
}

fn normalize(token: &str) -> Result<(u32, u32), ModelError> {
    if token.is_empty() || token.len() > MAX_MESSAGE_BYTES {
        return Err(ModelError::InvalidResponse);
    }
    let negative = token.starts_with('-');
    let unsigned = token.strip_prefix('-').unwrap_or(token);
    let (mantissa, exponent) = unsigned.split_once(['e', 'E']).unwrap_or((unsigned, "0"));
    let (integer, fraction) = match mantissa.split_once('.') {
        Some((integer, fraction)) if !fraction.is_empty() => (integer, fraction),
        Some(_) => return Err(ModelError::InvalidResponse),
        None => (mantissa, ""),
    };
    if integer.is_empty()
        || (integer.len() > 1 && integer.starts_with('0'))
        || !integer
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return Err(ModelError::InvalidResponse);
    }
    let exponent = parse_exponent(exponent)?;
    let digits = || integer.bytes().chain(fraction.bytes());
    let mut first = None;
    let mut last = 0;
    for (index, digit) in digits().enumerate() {
        if digit != b'0' {
            first.get_or_insert((index, digit));
            last = index;
        }
    }
    let Some((first, leading)) = first else {
        return Ok((0, PPM));
    };
    let decimal_position = integer.len() as i64 + exponent - first as i64;
    if negative
        || decimal_position > 1
        || (decimal_position == 1 && (leading != b'1' || last != first))
    {
        return Err(ModelError::InvalidResponse);
    }

    // Only seven digits contribute to ppm; later digits lower the complement by one millionth.
    let kept = decimal_position + 6;
    let mut yes = 0;
    if kept > 0 {
        for digit in digits().skip(first).take(kept as usize) {
            yes = yes * 10 + u32::from(digit - b'0');
        }
        let available = (integer.len() + fraction.len() - first) as i64;
        for _ in available..kept {
            yes *= 10;
        }
    }
    let remainder = last as i64 - first as i64 >= kept;
    // floor(PPM - x) = PPM - floor(x) - (x has a fractional part).
    Ok((yes, PPM - yes - u32::from(remainder)))
}

fn parse_exponent(token: &str) -> Result<i64, ModelError> {
    let negative = token.starts_with('-');
    let digits = token.strip_prefix(['-', '+']).unwrap_or(token);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ModelError::InvalidResponse);
    }
    // Exponents beyond the bounded mantissa plus ppm precision cannot change these results.
    let limit = MAX_MESSAGE_BYTES as i64 + 7;
    let magnitude = digits.bytes().fold(0_i64, |value, digit| {
        (value * 10 + i64::from(digit - b'0')).min(limit)
    });
    Ok(if negative { -magnitude } else { magnitude })
}
