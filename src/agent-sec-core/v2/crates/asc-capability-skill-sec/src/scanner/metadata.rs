//! Bounded YAML metadata decoding with V1 boolean and duplicate-key semantics.

use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use yaml_rust2::Yaml;
use yaml_rust2::parser::{Event, Parser, Tag};
use yaml_rust2::scanner::TScalarStyle;

// Metadata needs only a small tree. Count alias expansion as well as source nodes.
const MAX_NODES: usize = 10_000;
const MAX_DEPTH: usize = 32;

pub(super) fn parse(source: &str) -> Result<Value, ()> {
    let mut reader = Reader {
        parser: Parser::new_from_str(source),
        anchors: BTreeMap::new(),
        remaining: MAX_NODES,
        bytes: 8 * 1024 * 1024,
    };
    if reader.next()? != Event::StreamStart {
        return Err(());
    }
    match reader.next()? {
        Event::StreamEnd => Ok(Value::Null),
        Event::DocumentStart => {
            let event = reader.next()?;
            let value = reader.node(event, 0)?;
            if reader.next()? != Event::DocumentEnd || reader.next()? != Event::StreamEnd {
                return Err(());
            }
            Ok(value)
        }
        _ => Err(()),
    }
}

struct Reader<'a> {
    parser: Parser<std::str::Chars<'a>>,
    anchors: BTreeMap<usize, (Value, bool)>,
    remaining: usize,
    bytes: usize,
}

impl Reader<'_> {
    fn is_merge_key(&self, event: &Event) -> bool {
        match event {
            Event::Scalar(value, style, _, tag) => match tag {
                Some(tag) => is_merge_tag(tag),
                None => value == "<<" && *style == TScalarStyle::Plain,
            },
            Event::Alias(anchor) => self.anchors.get(anchor).is_some_and(|(_, merge)| *merge),
            _ => false,
        }
    }

    fn next(&mut self) -> Result<Event, ()> {
        self.parser
            .next_token()
            .map(|(event, _)| event)
            .map_err(|_| ())
    }

    fn node(&mut self, event: Event, depth: usize) -> Result<Value, ()> {
        self.remaining = self.remaining.checked_sub(1).ok_or(())?;
        if depth > MAX_DEPTH {
            return Err(());
        }
        let merge_key = self.is_merge_key(&event);
        let (value, anchor) = match event {
            Event::Scalar(value, style, anchor, tag) => {
                self.bytes = self.bytes.checked_sub(value.len()).ok_or(())?;
                (scalar(&value, style, tag.as_ref())?, anchor)
            }
            Event::SequenceStart(anchor, _) => {
                let mut values = Vec::new();
                loop {
                    let event = self.next()?;
                    if event == Event::SequenceEnd {
                        break;
                    }
                    values.push(self.node(event, depth + 1)?);
                }
                (Value::Array(values), anchor)
            }
            Event::MappingStart(anchor, _) => {
                let mut values = Map::new();
                let mut merges = Vec::new();
                loop {
                    let event = self.next()?;
                    if event == Event::MappingEnd {
                        break;
                    }
                    // Resolve merge semantics before JSON erases the scalar's style and tag.
                    let merge = self.is_merge_key(&event);
                    let key = self.node(event, depth + 1)?;
                    let event = self.next()?;
                    let value = self.node(event, depth + 1)?;
                    if merge {
                        merges.push(value);
                    } else {
                        values.insert(
                            key.as_str().map_or_else(|| key.to_string(), String::from),
                            value,
                        );
                    }
                }
                let mut combined = Map::new();
                for merge in merges {
                    merge_values(&mut combined, merge)?;
                }
                combined.extend(values);
                (Value::Object(combined), anchor)
            }
            Event::Alias(anchor) => {
                let (value, _) = self.anchors.get(&anchor).ok_or(())?;
                if depth + tree_depth(value) > MAX_DEPTH {
                    return Err(());
                }
                self.remaining = self.remaining.checked_sub(nodes(value)).ok_or(())?;
                self.bytes = self.bytes.checked_sub(bytes(value)).ok_or(())?;
                return Ok(value.clone());
            }
            _ => return Err(()),
        };
        if anchor != 0 {
            self.anchors.insert(anchor, (value.clone(), merge_key));
        }
        Ok(value)
    }
}

fn nodes(value: &Value) -> usize {
    1 + match value {
        Value::Array(values) => values.iter().map(nodes).sum(),
        Value::Object(values) => values.values().map(nodes).sum(),
        _ => 0,
    }
}

fn tree_depth(value: &Value) -> usize {
    match value {
        Value::Array(values) => 1 + values.iter().map(tree_depth).max().unwrap_or(0),
        Value::Object(values) => 1 + values.values().map(tree_depth).max().unwrap_or(0),
        _ => 0,
    }
}

fn bytes(value: &Value) -> usize {
    match value {
        Value::String(value) => value.len(),
        Value::Array(values) => values.iter().map(bytes).sum(),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| key.len() + bytes(value))
            .sum(),
        _ => 0,
    }
}

fn merge_values(target: &mut Map<String, Value>, value: Value) -> Result<(), ()> {
    match value {
        Value::Object(values) => target.extend(values),
        Value::Array(values) => {
            for value in values.into_iter().rev() {
                merge_values(target, value)?;
            }
        }
        _ => return Err(()),
    }
    Ok(())
}

fn is_merge_tag(tag: &Tag) -> bool {
    tag.handle == "tag:yaml.org,2002:" && tag.suffix == "merge"
}

fn scalar(value: &str, style: TScalarStyle, tag: Option<&Tag>) -> Result<Value, ()> {
    if tag.is_some_and(is_merge_tag) {
        return Ok(json!(value));
    }
    if style != TScalarStyle::Plain {
        return Ok(json!(value));
    }
    if let Some(tag) = tag {
        if tag.handle != "tag:yaml.org,2002:" {
            return Err(());
        }
        if tag.suffix == "str" || tag.suffix == "timestamp" {
            return Ok(json!(value));
        }
        if !["bool", "int", "float", "null"].contains(&tag.suffix.as_str()) {
            return Err(());
        }
    }
    // PyYAML's safe loader uses YAML 1.1; quoted words stay strings.
    match value {
        "yes" | "Yes" | "YES" | "true" | "True" | "TRUE" | "on" | "On" | "ON" => {
            return Ok(json!(true));
        }
        "no" | "No" | "NO" | "false" | "False" | "FALSE" | "off" | "Off" | "OFF" => {
            return Ok(json!(false));
        }
        _ => {}
    }
    if value.len() > 1
        && value.starts_with('0')
        && value.chars().all(|c| matches!(c, '0'..='7'))
        && let Ok(number) = i64::from_str_radix(value, 8)
    {
        return Ok(json!(number));
    }
    match Yaml::from_str(value) {
        Yaml::Null => Ok(Value::Null),
        Yaml::Boolean(value) => Ok(json!(value)),
        Yaml::Integer(value) => Ok(json!(value)),
        Yaml::Real(value) => Ok(value
            .parse::<f64>()
            .map_or_else(|_| json!(value), |n| json!(n))),
        _ => Ok(json!(value)),
    }
}
