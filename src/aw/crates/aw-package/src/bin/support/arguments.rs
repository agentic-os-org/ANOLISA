//! Decoding of process arguments shared by the build and install entrypoints.

use aw_package::{Error, Result};
use std::ffi::OsString;

/// Decode one process argument, rejecting non-UTF-8 input.
///
/// `std::env::args` instead panics on the first argument that is not valid
/// UTF-8, before any command validation runs, so both native entrypoints
/// decode `std::env::args_os` through this helper and report the rejected
/// invocation with the ordinary command error prefix and exit status.
pub fn decode(argument: OsString) -> Result<String> {
    argument
        .into_string()
        .map_err(|_| Error::Invalid("arguments must be valid UTF-8".into()))
}

/// Decode every remaining process argument before command parsing begins.
pub fn decode_all(arguments: impl Iterator<Item = OsString>) -> Result<Vec<String>> {
    arguments.map(decode).collect()
}
