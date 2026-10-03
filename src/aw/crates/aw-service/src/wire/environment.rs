//! Keep JSON string maps readable while preserving byte-valued Linux environments.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    os::unix::ffi::{OsStrExt, OsStringExt},
};

type Environment = BTreeMap<OsString, OsString>;

pub(super) fn serialize<S: Serializer>(
    environment: &Environment,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if let Some(text) = environment
        .iter()
        .map(|(key, value)| Some((key.to_str()?, value.to_str()?)))
        .collect::<Option<BTreeMap<_, _>>>()
    {
        text.serialize(serializer)
    } else {
        environment
            .iter()
            .map(|(key, value)| (key.as_bytes(), value.as_bytes()))
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
}

pub(super) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Environment, D::Error> {
    decode(deserializer, true)
}

fn decode<'de, D: Deserializer<'de>>(
    deserializer: D,
    validate_entries: bool,
) -> Result<Environment, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Encoded {
        Text(BTreeMap<String, String>),
        Bytes(Vec<(Vec<u8>, Vec<u8>)>),
    }
    let entries = match Encoded::deserialize(deserializer)? {
        Encoded::Text(text) => text
            .into_iter()
            .map(|(k, v)| (k.into_bytes(), v.into_bytes()))
            .collect(),
        Encoded::Bytes(bytes) => bytes,
    };
    let mut environment = BTreeMap::new();
    for (key, value) in entries {
        if validate_entries
            && (key.is_empty() || key.contains(&0) || key.contains(&b'=') || value.contains(&0))
        {
            return Err(serde::de::Error::custom("invalid environment entry"));
        }
        if environment
            .insert(OsString::from_vec(key), OsString::from_vec(value))
            .is_some()
        {
            return Err(serde::de::Error::custom("duplicate environment key"));
        }
    }
    Ok(environment)
}

pub(super) mod optional {
    use super::*;

    pub(crate) fn serialize<S: Serializer>(
        environment: &Option<Environment>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Encoded<'a>(#[serde(with = "super")] &'a Environment);
        environment.as_ref().map(Encoded).serialize(serializer)
    }

    // The Host validates native entries after claiming the step so malformed
    // snapshots follow on_error and produce the same audited failure as before.
    fn native_snapshot<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Environment, D::Error> {
        super::decode(deserializer, false)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Environment>, D::Error> {
        #[derive(Deserialize)]
        struct Encoded(#[serde(deserialize_with = "native_snapshot")] Environment);
        Ok(Option::<Encoded>::deserialize(deserializer)?.map(|value| value.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_entries_reject_invalid_or_duplicate_keys() {
        #[derive(Deserialize)]
        struct Fixture {
            #[serde(with = "super")]
            environment: Environment,
        }
        for entries in [
            "[[[],[]]]",
            "[[[65,0],[]]]",
            "[[[65,61],[]]]",
            "[[[65],[0]]]",
            "[[[65],[1]],[[65],[2]]]",
        ] {
            assert!(
                serde_json::from_str::<Fixture>(&format!(r#"{{"environment":{entries}}}"#))
                    .is_err()
            );
        }
        let text: Fixture = serde_json::from_str(r#"{"environment":{"A":"ok"}}"#).unwrap();
        assert_eq!(text.environment[std::ffi::OsStr::new("A")], "ok");
    }
}
