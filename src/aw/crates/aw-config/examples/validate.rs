//! Offline developer example; this is not the planned AW product CLI.

use aw_config::{Validator, MAX_DOCUMENT_BYTES};
use std::{env, error::Error, fs::File, io::Read};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let path = args.next().ok_or("usage: validate <configuration.yaml>")?;
    if args.next().is_some() {
        return Err("usage: validate <configuration.yaml>".into());
    }
    let mut input = Vec::new();
    File::open(path)?
        .take(MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut input)?;
    Validator::new()?.parse(&input)?;
    println!("Configuration is statically valid; runtime admission has not run.");
    Ok(())
}
