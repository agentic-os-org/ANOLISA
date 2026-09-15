//! v1 structural validators; JWT validation does not verify a signature.

use crate::python_unicode::DECIMAL_RANGES;
use base64::Engine;
use base64::engine::{
    DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig, general_purpose::URL_SAFE_NO_PAD,
};
use chrono::NaiveDate;
use std::collections::BTreeMap;

pub(crate) fn decimal(c: char) -> Option<u32> {
    // Adjacent decimal alphabets each contain 0..9; some ranges combine several.
    let index = DECIMAL_RANGES.partition_point(|(_, end)| *end < c);
    let (start, _) = *DECIMAL_RANGES.get(index)?;
    (start <= c).then(|| (u32::from(c) - u32::from(start)) % 10)
}

pub(crate) fn luhn(value: &str) -> bool {
    let digits: Vec<_> = value.chars().filter_map(decimal).collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    let parity = digits.len() % 2;
    digits
        .iter()
        .enumerate()
        .map(|(index, digit)| {
            let n = if index % 2 == parity {
                digit * 2
            } else {
                *digit
            };
            if n > 9 { n - 9 } else { n }
        })
        .sum::<u32>()
        % 10
        == 0
}

pub(crate) fn cn_id(value: &str) -> bool {
    let chars: Vec<_> = value.chars().collect();
    if chars.len() != 18 {
        return false;
    }
    let Some(digits): Option<Vec<_>> = chars[..17].iter().copied().map(decimal).collect() else {
        return false;
    };
    // Python strptime accepts Unicode year digits, ASCII month digits, and
    // a Unicode units digit only in days beginning with ASCII 0, 1, or 2.
    if !chars[10..12].iter().all(char::is_ascii_digit)
        || !matches!(chars[12], '0'..='3')
        || (chars[12] == '3' && !matches!(chars[13], '0' | '1'))
    {
        return false;
    }
    let year = digits[6..10].iter().fold(0, |n, d| n * 10 + d);
    let month = digits[10] * 10 + digits[11];
    let day = digits[12] * 10 + digits[13];
    if year == 0 || NaiveDate::from_ymd_opt(i32::try_from(year).unwrap_or(0), month, day).is_none()
    {
        return false;
    }
    let weights = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
    let sum: u32 = digits.iter().zip(weights).map(|(d, w)| d * w).sum();
    let checks = ['1', '0', 'X', '9', '8', '7', '6', '5', '4', '3', '2'];
    checks.get(usize::try_from(sum % 11).unwrap_or(0)).copied()
        == Some(chars[17].to_ascii_uppercase())
}

pub(crate) fn email(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    if value.len() > 254
        || local.is_empty()
        || local.len() > 64
        || domain.len() > 253
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
    {
        return false;
    }
    if !local
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b"._%+-".contains(&c))
    {
        return false;
    }
    let labels: Vec<_> = domain.split('.').collect();
    let Some(tld) = labels.last() else {
        return false;
    };
    labels.len() >= 2
        && (2..=63).contains(&tld.len())
        && tld.bytes().all(|c| c.is_ascii_alphabetic())
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
}

pub(crate) fn jwt(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|s| {
            s.is_empty()
                || !s
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        })
    {
        return false;
    }
    let signature_engine = GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::RequireNone)
            .with_decode_allow_trailing_bits(true),
    );
    let Ok(signature) = signature_engine.decode(parts[2]) else {
        return false;
    };
    if signature.is_empty() {
        return false;
    }
    let mut decoded = Vec::new();
    for segment in &parts[..2] {
        let Ok(bytes) = URL_SAFE_NO_PAD.decode(segment) else {
            return false;
        };
        if bytes.is_empty() || URL_SAFE_NO_PAD.encode(&bytes) != *segment {
            return false;
        }
        let Some(json) = structural_json(bytes) else {
            return false;
        };
        if !json.trim_start().starts_with('{')
            || serde_json::from_str::<serde::de::IgnoredAny>(&json).is_err()
        {
            return false;
        }
        decoded.push(json);
    }
    // Raw values validate syntax without interpreting numbers or recursively
    // constructing claims. Collecting a map preserves Python's last-key-wins.
    let Ok(header) =
        serde_json::from_str::<BTreeMap<String, &serde_json::value::RawValue>>(&decoded[0])
    else {
        return false;
    };
    header
        .get("alg")
        .and_then(|value| serde_json::from_str::<String>(value.get()).ok())
        .is_some_and(|s| {
            !s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
                .is_empty()
        })
}

// V1's json.loads accepts non-finite tokens and escaped UTF-16 surrogates.
// Only object shape and a nonblank alg string affect detection: normalize those
// extensions for serde's syntax check, never exposing or trusting decoded claims.
fn structural_json(mut bytes: Vec<u8>) -> Option<String> {
    let mut index = 0;
    let mut quoted = false;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => quoted = !quoted,
            b'\\' if quoted => {
                if bytes.get(index + 1) == Some(&b'u')
                    && let Some(hex) = bytes.get(index + 2..index + 6)
                    && let Ok(hex) = std::str::from_utf8(hex)
                    && let Ok(value) = u16::from_str_radix(hex, 16)
                    && (0xd800..=0xdfff).contains(&value)
                {
                    // A surrogate is never whitespace or part of the "alg" key.
                    bytes[index + 2..index + 6].copy_from_slice(b"FFFD");
                }
                index += 1;
            }
            _ if !quoted => {
                for token in [b"NaN".as_slice(), b"Infinity", b"-Infinity"] {
                    if bytes[index..].starts_with(token)
                        && (index == 0 || b" \r\n\t[:,".contains(&bytes[index - 1]))
                        && bytes
                            .get(index + token.len())
                            .is_none_or(|b| b" \r\n\t,]}".contains(b))
                    {
                        bytes[index..index + token.len()].fill(b' ');
                        bytes[index] = b'0';
                        index += token.len() - 1;
                        break;
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn token(header: &serde_json::Value, payload: &serde_json::Value) -> String {
        format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(payload.to_string()),
            URL_SAFE_NO_PAD.encode([0; 32])
        )
    }

    #[test]
    fn python_json_extensions_preserve_detection_without_relaxing_json_syntax() {
        for (header, payload, valid) in [
            (r#"{"alg":"HS256"}"#, r#"{"claim":NaN}"#, true),
            (r#"{"alg":"HS256"}"#, r#"{"claim":-Infinity}"#, true),
            (r#"{"alg":"HS256"}"#, r#"{"claim":1e999}"#, true),
            (r#"{"alg":"\ud800"}"#, "{}", true),
            (r#"{"alg":1e999,"alg":"HS256"}"#, "{}", true),
            (r#"{"alg":"HS256","alg":""}"#, "{}", false),
            (r#"{"alg":"HS256"}"#, r#"{"claim":-NaN}"#, false),
            (r#"{"alg":"HS256"}"#, r#"{"claim":+Infinity}"#, false),
            (r#"{"alg":"HS256"}"#, r#"{"claim":NaNtrue}"#, false),
            (r#"{"alg":"HS256"}"#, r#"{"claim":Infinity.0}"#, false),
        ] {
            let token = format!(
                "{}.{}.AA",
                URL_SAFE_NO_PAD.encode(header),
                URL_SAFE_NO_PAD.encode(payload)
            );
            assert_eq!(jwt(&token), valid, "{header} {payload}");
        }
    }

    #[test]
    fn jwt_validates_structure_and_canonical_json_not_signature_authenticity() {
        let valid = token(&json!({"alg":"HS256"}), &json!({"sub":"123"}));
        assert!(jwt(&valid));
        let parts: Vec<_> = valid.split('.').collect();
        let signature_alias = format!("{}B", &parts[2][..parts[2].len() - 1]);
        assert!(jwt(&format!(
            "{}.{}.{}",
            parts[0], parts[1], signature_alias
        )));
        let payload_alias = format!("{}R", &parts[1][..parts[1].len() - 1]);
        assert!(!jwt(&format!(
            "{}.{}.{}",
            parts[0], payload_alias, parts[2]
        )));
        for invalid in [
            token(&json!({"typ":"JWT"}), &json!({})),
            token(&json!({"alg":"  "}), &json!({})),
            token(&json!({"alg":"\u{1c}\u{1f}"}), &json!({})),
            token(&json!({"alg":"HS256"}), &json!([])),
            "not.a.jwt".into(),
            format!("{}.{}.a", parts[0], parts[1]),
            format!(
                "{}.{}.{}",
                parts[0],
                URL_SAFE_NO_PAD.encode("[".repeat(1100)),
                parts[2]
            ),
        ] {
            assert!(!jwt(&invalid));
        }
    }

    #[test]
    fn email_and_card_validation_reject_plausible_invalid_values() {
        assert!(luhn("4111 1111 1111 1111"));
        assert!(luhn("４１１１ １１１１ １１１１ １１１１"));
        assert!(!luhn("4111111111111112"));
        for valid in [
            "alice@company.cn",
            "first.last+tag@sub-domain.company.cn",
            "ALICE_01@SECURECORP.COM",
        ] {
            assert!(email(valid));
        }
        for invalid in [
            ".alice@company.cn",
            "alice.@company.cn",
            "alice..bob@company.cn",
            "alice@-company.cn",
            "alice@company-.cn",
            "alice@bad_domain.cn",
            "alice@company..cn",
            "alice@localhost",
            "alice@company.123",
        ] {
            assert!(!email(invalid));
        }
        assert!(!email(&format!("{}@company.cn", "a".repeat(65))));
    }

    #[test]
    fn id_checks_date_checksum_and_v1_unicode_date_syntax() {
        assert!(cn_id("11010519491231002X"));
        assert!(cn_id("11010519491231002x"));
        assert!(cn_id("110105１９４９1231002X"));
        assert!(!cn_id("1101051949１２31002X"));
        assert!(!cn_id("110105194912３１002X"));
        assert!(!cn_id("1101051949123１002X"));
        assert!(!cn_id("11010519490231002X"));
        assert!(!cn_id("110105194912310021"));
    }
}
