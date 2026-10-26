//! Keep exact UTF-8 database bytes compact while accepting earlier journal byte arrays.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Deserialize)]
#[serde(untagged)]
enum Stored {
    Text(String),
    Bytes(Vec<u8>),
}

impl Stored {
    fn bytes(self) -> Vec<u8> {
        match self {
            Self::Text(text) => text.into_bytes(),
            Self::Bytes(bytes) => bytes,
        }
    }
}

pub(super) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    serializer
        .serialize_str(std::str::from_utf8(bytes).map_err(<S::Error as serde::ser::Error>::custom)?)
}

pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    Ok(Stored::deserialize(deserializer)?.bytes())
}

pub(super) mod optional {
    use super::*;

    pub(crate) fn serialize<S: Serializer>(
        bytes: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        bytes
            .as_deref()
            .map(std::str::from_utf8)
            .transpose()
            .map_err(<S::Error as serde::ser::Error>::custom)?
            .serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        Ok(Option::<Stored>::deserialize(deserializer)?.map(Stored::bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Journal, MAX_JOURNAL_BYTES};
    use crate::database::{DatabasePaths, InstalledDatabase, MAX_DATABASE_BYTES};
    use serde::{Deserialize, Serialize};
    use std::{collections::BTreeMap, path::Path};

    #[test]
    fn maximum_database_snapshots_fit_the_journal_and_preserve_exact_bytes() {
        let snapshot = |generation| {
            let mut bytes = serde_json::to_vec(&InstalledDatabase {
                schema_version: 1,
                generation,
                installations: Vec::new(),
            })
            .unwrap();
            bytes.resize(MAX_DATABASE_BYTES as usize, b' ');
            bytes
        };
        let paths = DatabasePaths::user(Some(Path::new("/tmp/state")), None).unwrap();
        let entry = Journal {
            schema_version: 1,
            transaction_id: "txn-1-2-3".into(),
            database_root: paths.root.clone(),
            prefix: "/tmp/aw-payload".into(),
            installation_id: "example".into(),
            before_database: Some(snapshot(0)),
            after_database: snapshot(1),
            prefix_created: true,
            prefix_identity: None,
            workspace_identity: None,
            lock_created: true,
            lock_identity: None,
            directories: BTreeMap::new(),
            actions: Vec::new(),
            database_stage: None,
        };
        entry.validate(&paths, &entry.directory()).unwrap();
        let encoded = entry.bytes().unwrap();
        assert!((encoded.len() as u64) < MAX_JOURNAL_BYTES);
        let decoded: Journal = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.before_database, entry.before_database);
        assert_eq!(decoded.after_database, entry.after_database);
        decoded.validate(&paths, &decoded.directory()).unwrap();
    }

    #[test]
    fn previous_byte_array_snapshots_remain_readable() {
        #[derive(Deserialize, Serialize)]
        struct Snapshot {
            #[serde(with = "super::optional")]
            before: Option<Vec<u8>>,
            #[serde(with = "super")]
            after: Vec<u8>,
        }
        let decoded: Snapshot =
            serde_json::from_str(r#"{"before":[123,125],"after":[123,125]}"#).unwrap();
        assert_eq!(decoded.before.as_deref(), Some(b"{}".as_slice()));
        assert_eq!(decoded.after, b"{}");
        let encoded = serde_json::to_value(decoded).unwrap();
        assert_eq!(encoded["before"], "{}");
        assert_eq!(encoded["after"], "{}");
    }
}
