//! `anolisa env` — print detected environment facts.
//!
//! Default (human) output prints one `key: value` line per field so that
//! downstream shell consumers can `grep`/`awk` without dragging in a
//! JSON parser. `--json` emits the standard [`crate::response`] envelope
//! with the full serialized [`anolisa_env::EnvFacts`] as `data`.

use clap::Parser;

use crate::color::Palette;
use crate::context::CliContext;
use crate::response::{CliError, render_json};

#[derive(Parser)]
pub struct EnvArgs {
    /// Include all probe details (reserved for richer human output).
    #[arg(long)]
    pub verbose: bool,
}

pub fn handle(args: EnvArgs, ctx: &CliContext) -> Result<(), CliError> {
    let facts = anolisa_env::EnvService::detect();

    if ctx.json {
        let data = serde_json::to_value(&facts).map_err(|e| CliError::InvalidArgument {
            command: "env".to_string(),
            reason: format!("failed to serialize env facts: {e}"),
        })?;
        return render_json("env", data);
    }

    // Human path — `verbose` is accepted but not yet differentiated from
    // the default summary. Reserved for future use; kept on the flag set
    // so the CLI surface contract stays stable.
    let _ = args.verbose;
    let _ = ctx.verbose;

    let color = Palette::new(ctx.no_color);
    println!("{} {}", color.label("os:"), facts.os);
    println!("{} {}", color.label("arch:"), facts.arch);
    println!(
        "{} {}",
        color.label("libc:"),
        display_opt_str(facts.libc.as_deref())
    );
    println!(
        "{} {}",
        color.label("kernel:"),
        display_opt_str(facts.kernel.as_deref())
    );
    println!(
        "{} {}",
        color.label("pkg_base:"),
        display_opt_str(facts.pkg_base.as_deref())
    );
    println!(
        "{} {}",
        color.label("btf:"),
        color.bool_value(display_opt_bool(facts.btf))
    );
    println!(
        "{} {}",
        color.label("cap_bpf:"),
        color.bool_value(display_opt_bool(facts.cap_bpf))
    );
    println!(
        "{} {}",
        color.label("container:"),
        display_opt_str(facts.container.as_deref())
    );
    println!("{} {}", color.label("user:"), facts.user);
    println!("{} {}", color.label("uid:"), facts.uid);
    println!(
        "{} {}",
        color.label("home:"),
        color.path(facts.home.display())
    );
    Ok(())
}

fn display_opt_str(v: Option<&str>) -> String {
    v.map(|s| s.to_string()).unwrap_or_else(|| "unknown".into())
}

fn display_opt_bool(v: Option<bool>) -> String {
    v.map(|b| b.to_string()).unwrap_or_else(|| "unknown".into())
}
