//! Read the portable Agent Skills fields without applying legacy defaults.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fmt;
use std::sync::LazyLock;

use regex::Regex;

use serde::Deserializer;
use serde::de::{DeserializeSeed, Error, IgnoredAny, MapAccess, Visitor};
use unicode_normalization::UnicodeNormalization;

const FIELDS: &[&str] = &[
    "name",
    "description",
    "license",
    "compatibility",
    "metadata",
    "allowed-tools",
];

pub(super) struct InvalidFrontmatter {
    pub(super) field: Option<String>,
    pub(super) message: &'static str,
}

impl InvalidFrontmatter {
    fn new(field: Option<&str>, message: &'static str) -> Self {
        Self {
            field: field.map(str::to_owned),
            message,
        }
    }
}

pub(super) fn validate(yaml: &str, leaf: &str) -> Result<String, InvalidFrontmatter> {
    let mut invalid = None;
    let fields = serde_yaml::Deserializer::from_str(yaml)
        .deserialize_any(FieldNames(&mut invalid))
        .map_err(|error| {
            invalid.unwrap_or_else(|| {
                let message = if error.location().is_some() {
                    "invalid YAML frontmatter"
                } else {
                    "expected a YAML mapping"
                };
                InvalidFrontmatter::new(None, message)
            })
        })?;
    if let Some(field) = fields
        .iter()
        .find(|field| !FIELDS.contains(&field.as_str()))
    {
        return Err(InvalidFrontmatter::new(
            Some(field),
            "unknown Agent Skills frontmatter field",
        ));
    }

    let mut invalid = None;
    let values = serde_yaml::Deserializer::from_str(yaml)
        .deserialize_map(Values(&mut invalid))
        .map_err(|_| {
            invalid.unwrap_or_else(|| InvalidFrontmatter::new(None, "invalid YAML frontmatter"))
        })?;

    static NAME: LazyLock<Result<Regex, regex::Error>> =
        LazyLock::new(|| Regex::new(r"\A[\p{L}\p{N}](?:[\p{L}\p{N}-]*[\p{L}\p{N}])?\z"));
    let name: String = values.name.nfkc().collect();
    if !(1..=64).contains(&name.chars().count())
        || !NAME.as_ref().is_ok_and(|pattern| pattern.is_match(&name))
        || name.contains("--")
        || name
            .chars()
            .any(|c| !c.to_lowercase().eq(std::iter::once(c)))
    {
        return Err(InvalidFrontmatter::new(
            Some("name"),
            "invalid Agent Skills name",
        ));
    }
    if !name.chars().eq(leaf.nfkc()) {
        return Err(InvalidFrontmatter::new(
            Some("name"),
            "name differs from its parent directory",
        ));
    }
    if values.description.trim().is_empty() || values.description.chars().count() > 1024 {
        return Err(InvalidFrontmatter::new(
            Some("description"),
            "description must contain 1–1024 characters",
        ));
    }
    if values
        .compatibility
        .is_some_and(|value| !(1..=500).contains(&value.chars().count()))
    {
        return Err(InvalidFrontmatter::new(
            Some("compatibility"),
            "compatibility must contain 1–500 characters",
        ));
    }
    Ok(values.description)
}

pub(super) fn normalize_line_endings(text: &str) -> Cow<'_, str> {
    if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

pub(super) fn split(text: &str) -> Result<(&str, &str), InvalidFrontmatter> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    let fence = |line: &str| line.trim_end_matches([' ', '\t', '\r', '\n']) == "---";
    let opening = lines
        .next()
        .filter(|line| fence(line))
        .ok_or_else(|| InvalidFrontmatter::new(None, "missing opening frontmatter delimiter"))?;
    let mut end = opening.len();
    for line in lines {
        if fence(line) {
            return Ok((&text[opening.len()..end], &text[end + line.len()..]));
        }
        end += line.len();
    }
    Err(InvalidFrontmatter::new(
        None,
        "missing closing frontmatter delimiter",
    ))
}

struct FieldNames<'a>(&'a mut Option<InvalidFrontmatter>);

impl<'de> Visitor<'de> for FieldNames<'_> {
    type Value = BTreeSet<String>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a YAML mapping")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut fields = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if fields.contains(&key) {
                *self.0 = Some(InvalidFrontmatter::new(Some(&key), "duplicate YAML field"));
                return Err(A::Error::custom("duplicate YAML field"));
            }
            fields.insert(key);
            map.next_value::<IgnoredAny>()?;
        }
        Ok(fields)
    }
}

#[derive(Default)]
struct RequiredValues {
    name: String,
    description: String,
    compatibility: Option<String>,
}

struct Values<'a>(&'a mut Option<InvalidFrontmatter>);

impl<'de> Visitor<'de> for Values<'_> {
    type Value = RequiredValues;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Agent Skills fields")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut values = RequiredValues::default();
        while let Some(key) = map.next_key::<String>()? {
            let mut message = "expected a string";
            let result = if key == "metadata" {
                message = "expected a YAML mapping";
                map.next_value_seed(StringMap(&mut message))
            } else {
                // Request a scalar string directly to retain the spelling of
                // bare bool-like, number-like and null-like values.
                map.next_value::<String>().map(|value| match key.as_str() {
                    "name" => values.name = value,
                    "description" => values.description = value,
                    "compatibility" => values.compatibility = Some(value),
                    _ => {}
                })
            };
            if let Err(error) = result {
                *self.0 = Some(InvalidFrontmatter::new(Some(&key), message));
                return Err(error);
            }
        }
        Ok(values)
    }
}

struct StringMap<'a>(&'a mut &'static str);

impl<'de> DeserializeSeed<'de> for StringMap<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for StringMap<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a mapping of scalar strings")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut keys = BTreeSet::new();
        *self.0 = "expected a string";
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                *self.0 = "duplicate YAML field";
                return Err(A::Error::custom("duplicate YAML field"));
            }
            map.next_value::<String>()?;
        }
        Ok(())
    }
}
