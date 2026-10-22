//! Reject non-UTF-8 compatibility arguments before parsing or touching files.

use aw_package::{Error, Result};
use std::ffi::OsString;

pub(super) fn decode_all(arguments: impl Iterator<Item = OsString>) -> Result<Vec<String>> {
    arguments
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| Error::Invalid("arguments must be valid UTF-8".into()))
        })
        .collect()
}
