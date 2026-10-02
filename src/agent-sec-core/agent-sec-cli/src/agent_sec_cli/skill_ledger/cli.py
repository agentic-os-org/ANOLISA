"""skill-ledger CLI — Typer subcommand group.

Mounted as a subcommand group under ``agent-sec-cli skill-ledger <cmd>``.
Stateful commands route through ``invoke("skill_ledger", command=..., ...)``;
``analyze`` intentionally bypasses that lifecycle to remain side-effect free.
"""

import getpass
import json
import os
from typing import Optional

import typer
from agent_sec_cli import __version__ as AGENT_SEC_VERSION
from agent_sec_cli.security_middleware import invoke
from agent_sec_cli.security_middleware.result import ActionResult
from agent_sec_cli.skill_ledger.activation_policy import (
    validate_activation_policy,
)
from agent_sec_cli.skill_ledger.scanner.names import validate_scanner_name

app = typer.Typer(
    name="skill-ledger",
    help=(
        "Skill security management — track changes, verify integrity, and sign skills.\n\n"
        "Typical workflow:\n\n"
        "  1. analyze   Read-only content analysis without ledger state\n"
        "  2. init      Initialize keys and baseline covered skills\n"
        "  3. scan      Run built-in scanners and sign the manifest\n"
        "  4. check     Verify a skill's integrity status\n"
        "  5. certify   Import external findings and sign the manifest\n"
        "  6. status    Show overall ledger health overview\n"
        "  7. audit     Deep-verify the full version history\n\n"
        "Integrity states:\n\n"
        "  pass      Files unchanged, signature valid, scan clean\n"
        "  none      No ledger artifacts, or verified scan status is none\n"
        "  drifted   Skill files changed since last certification\n"
        "  warn      Scan found low-risk issues\n"
        "  deny      Scan found high-risk issues\n"
        "  tampered  Ledger metadata is incomplete or fails authenticity checks\n\n"
        "Other command outcomes:\n\n"
        "  unmanaged Skill directory is not covered by any managed skill\n"
        "            directory, or ledger state is not writable\n"
        "  error     Check could not be completed for the skill"
    ),
    add_completion=True,
)


# ---------------------------------------------------------------------------
# Shared helper
# ---------------------------------------------------------------------------


def _forward(result: ActionResult) -> None:
    """Print ActionResult stdout/error and exit with its exit_code."""
    if result.stdout:
        typer.echo(result.stdout, nl=False)
    warnings = result.data.get("warnings", [])
    if isinstance(warnings, list):
        for warning in warnings:
            typer.echo(str(warning), err=True)
    if result.error:
        typer.echo(result.error, err=True)
    raise typer.Exit(code=result.exit_code)


def _parse_scanner_names(scanners: Optional[str]) -> list[str] | None:
    """Parse a comma-separated scanner list."""
    names = [s.strip() for s in scanners.split(",") if s.strip()] if scanners else None
    for name in names or []:
        try:
            validate_scanner_name(name)
        except ValueError as exc:
            raise typer.BadParameter(str(exc), param_hint="--scanners") from exc
    return names


def _validate_cli_scanner(value: str) -> str:
    try:
        return validate_scanner_name(value)
    except ValueError as exc:
        raise typer.BadParameter(str(exc)) from exc


def _validate_cli_policy(value: str | None) -> str | None:
    if value is None:
        return None
    try:
        return validate_activation_policy(value)
    except ValueError as exc:
        raise typer.BadParameter(str(exc)) from exc


def _resolve_new_key_passphrase(use_passphrase: bool) -> str | None:
    """Resolve --passphrase using env first, then interactive prompts."""
    passphrase: str | None = None
    if use_passphrase:
        env_pass = os.environ.get("SKILL_LEDGER_PASSPHRASE")
        if env_pass is not None:
            # Use ``is not None`` so that SKILL_LEDGER_PASSPHRASE="" is
            # accepted (treated as "no passphrase" — unencrypted keys).
            passphrase = env_pass if env_pass else None
        else:
            passphrase = getpass.getpass("Enter passphrase for new signing key: ")
            confirm = getpass.getpass("Confirm passphrase: ")
            if passphrase != confirm:
                typer.echo("Error: passphrases do not match", err=True)
                raise typer.Exit(code=1)
            if not passphrase:
                typer.echo("Error: passphrase cannot be empty", err=True)
                raise typer.Exit(code=1)
    return passphrase


# ---------------------------------------------------------------------------
# init
# ---------------------------------------------------------------------------


@app.command("init")
def cmd_init(
    no_baseline: bool = typer.Option(
        False,
        "--no-baseline",
        help="Only initialize keys; do not scan covered skills.",
    ),
    use_passphrase: bool = typer.Option(
        False,
        "--passphrase",
        help="Protect a newly-created private key with a passphrase.",
    ),
    force_keys: bool = typer.Option(
        False,
        "--force-keys",
        help="Overwrite existing keys (old public key is archived).",
    ),
    scanners: Optional[str] = typer.Option(
        None,
        "--scanners",
        help="Comma-separated built-in scanners for baseline scan (default: code-scanner,static-scanner).",
    ),
) -> None:
    """Initialize skill-ledger and baseline covered skills."""
    scanner_names = _parse_scanner_names(scanners)
    passphrase = _resolve_new_key_passphrase(use_passphrase)
    result = invoke(
        "skill_ledger",
        command="init",
        baseline=not no_baseline,
        passphrase=passphrase,
        passphrase_requested=use_passphrase,
        force_keys=force_keys,
        scanner_names=scanner_names,
    )
    _forward(result)


# ---------------------------------------------------------------------------
# check
# ---------------------------------------------------------------------------


@app.command("check")
def cmd_check(
    skill_dir: Optional[str] = typer.Argument(
        None, help="Path to the skill directory to check (omit when using --all)"
    ),
    all_skills: bool = typer.Option(
        False,
        "--all",
        help="Check every registered skill at once.",
    ),
) -> None:
    """Check a skill's integrity and output its security status as JSON.

    Compares current file hashes against the signed manifest and verifies
    the digital signature. Possible statuses:

      pass      Files unchanged, signature valid, scan clean
      none      No ledger artifacts, or verified scan status is none
      drifted   Skill files changed since last certification
      warn      Signature valid, but scan found low-risk issues
      deny      Signature valid, but scan found high-risk issues
      tampered  Ledger metadata is incomplete or fails authenticity checks
      error     Check could not run for the skill (e.g. missing SKILL.md);
                single checks print a JSON object with status and error
                keys and exit 1, while --all records it as a per-skill
                entry and checks the rest

    Use --all to check every registered skill and receive a JSON array of
    enriched results. Skill discovery uses built-in default directories plus
    ~/.config/agent-sec/skill-ledger/config.json managedSkillDirs (paths and
    globs expanded automatically by the CLI). Set enableDefaultSkillDirs=false
    in config.json for isolated runs that should ignore built-in defaults.
    """
    if all_skills and skill_dir is not None:
        typer.echo(
            "Error: --all and skill_dir are mutually exclusive.",
            err=True,
        )
        raise typer.Exit(code=1)

    result = invoke(
        "skill_ledger",
        command="check",
        skill_dir=skill_dir,
        all_skills=all_skills,
    )
    _forward(result)


# ---------------------------------------------------------------------------
# analyze
# ---------------------------------------------------------------------------


@app.command("analyze")
def cmd_analyze(
    skill_dir: Optional[str] = typer.Argument(
        None,
        help="Path to the Skill directory to analyze.",
    ),
    output_format: str = typer.Option(
        "json",
        "--format",
        help="Output format. Version 1 supports only 'json'.",
    ),
) -> None:
    """Analyze current Skill content without creating or updating Skill Ledger state."""
    from agent_sec_cli.skill_ledger.analyze import (  # noqa: PLC0415
        SCHEMA_VERSION,
        analyze_skill,
    )

    if output_format.lower() != "json":
        payload = {
            "schema_version": SCHEMA_VERSION,
            "engine_version": AGENT_SEC_VERSION,
            "status": "error",
            "coverage_complete": False,
            "scanners": [],
            "errors": [
                {
                    "code": "unsupported-format",
                    "message": "Output format must be 'json'.",
                }
            ],
        }
        typer.echo(json.dumps(payload, ensure_ascii=False, sort_keys=False))
        raise typer.Exit(code=2)
    if skill_dir is None:
        payload = {
            "schema_version": SCHEMA_VERSION,
            "engine_version": AGENT_SEC_VERSION,
            "status": "error",
            "coverage_complete": False,
            "scanners": [],
            "errors": [
                {
                    "code": "skill-root-required",
                    "message": "Skill root argument is required.",
                }
            ],
        }
        typer.echo(json.dumps(payload, ensure_ascii=False, sort_keys=False))
        raise typer.Exit(code=2)

    payload, exit_code = analyze_skill(skill_dir)
    typer.echo(json.dumps(payload, ensure_ascii=False, sort_keys=False))
    raise typer.Exit(code=exit_code)


# ---------------------------------------------------------------------------
# scan
# ---------------------------------------------------------------------------


@app.command("scan")
def cmd_scan(
    skill_dir: Optional[str] = typer.Argument(
        None, help="Path to the skill directory to scan (omit when using --all)"
    ),
    all_skills: bool = typer.Option(
        False,
        "--all",
        help="Scan every registered skill using fill-in behavior.",
    ),
    force: bool = typer.Option(
        False,
        "--force",
        help="Re-run requested scanners even when matching results already exist.",
    ),
    scanners: Optional[str] = typer.Option(
        None,
        "--scanners",
        help="Comma-separated built-in scanner names (default: code-scanner,static-scanner).",
    ),
) -> None:
    """Run built-in scanners and record signed scan results."""
    if all_skills and skill_dir is not None:
        typer.echo(
            "Error: --all and skill_dir are mutually exclusive.",
            err=True,
        )
        raise typer.Exit(code=1)

    result = invoke(
        "skill_ledger",
        command="scan",
        skill_dir=skill_dir,
        all_skills=all_skills,
        force=force,
        scanner_names=_parse_scanner_names(scanners),
    )
    _forward(result)


# ---------------------------------------------------------------------------
# certify
# ---------------------------------------------------------------------------


@app.command("certify")
def cmd_certify(
    skill_dir: str = typer.Argument(..., help="Path to the skill directory"),
    findings: Optional[str] = typer.Option(
        None,
        "--findings",
        help="Path to a findings JSON file from an external scanner (e.g., skill-vetter)",
    ),
    scanner: str = typer.Option(
        "skill-vetter",
        "--scanner",
        callback=_validate_cli_scanner,
        help="Name of the scanner that produced the findings file",
    ),
    scanner_version: Optional[str] = typer.Option(
        None,
        "--scanner-version",
        help="Version of the scanner that produced the findings",
    ),
    delete_findings: bool = typer.Option(
        False,
        "--delete-findings",
        help="Delete the findings file after a successful import.",
    ),
) -> None:
    """Import external scanner findings into a signed manifest."""
    if findings is None:
        typer.echo(
            "Error: --findings is required for certify. Use 'skill-ledger scan' for built-in scanners.",
            err=True,
        )
        raise typer.Exit(code=1)

    result = invoke(
        "skill_ledger",
        command="certify",
        skill_dir=skill_dir,
        findings=findings,
        scanner=scanner,
        scanner_version=scanner_version,
        delete_findings=delete_findings,
    )
    _forward(result)


# ---------------------------------------------------------------------------
# status
# ---------------------------------------------------------------------------


@app.command("status")
def cmd_status(
    verbose: bool = typer.Option(
        False,
        "--verbose",
        "-v",
        help="Include per-skill results array in the output.",
    ),
) -> None:
    """Show an overview of the skill-ledger system health.

    Reports signing key infrastructure, configuration state, and
    aggregate integrity status across all registered skills.

    Output is a single JSON object with three sections:

      keys     Signing key status (initialized, fingerprint, encrypted)
      config   Configuration summary (default/managed skill dirs, scanners)
      skills   Aggregate health (discovered count, per-status breakdown)

    Use --verbose to include the full per-skill results array.
    For per-skill integrity checks use the 'check' command instead.
    """
    result = invoke(
        "skill_ledger",
        command="status",
        verbose=verbose,
    )
    _forward(result)


# ---------------------------------------------------------------------------
# audit
# ---------------------------------------------------------------------------


@app.command("audit")
def cmd_audit(
    skill_dir: str = typer.Argument(..., help="Path to the skill directory to audit"),
    verify_snapshots: bool = typer.Option(
        False,
        "--verify-snapshots",
        help="Also verify that snapshot file hashes match stored records",
    ),
) -> None:
    """Verify the full version-chain integrity for a skill.

    Walks every historical version in .skill-meta/versions/ and checks:

      - Hash consistency (file hashes match the recorded values)
      - Signature validity (each version's digital signature is correct)
      - Chain linkage (each version references the previous signature)

    Use --verify-snapshots to additionally validate snapshot file hashes
    against the stored records — useful for detecting silent file corruption.
    """
    result = invoke(
        "skill_ledger",
        command="audit",
        skill_dir=skill_dir,
        verify_snapshots=verify_snapshots,
    )
    _forward(result)


# ---------------------------------------------------------------------------
# list-scanners
# ---------------------------------------------------------------------------


@app.command("list-scanners")
def cmd_list_scanners() -> None:
    """List registered scanners and their configuration.

    Shows all scanners defined in the built-in defaults and
    ~/.config/agent-sec/skill-ledger/config.json, including their invocation type,
    result parser, and enabled status.

    Use this to discover valid values for scan --scanners and certify --scanner.
    """
    result = invoke("skill_ledger", command="list-scanners")
    _forward(result)


# ---------------------------------------------------------------------------
# decide
# ---------------------------------------------------------------------------


@app.command("decide")
def cmd_decide(
    skill_dir: str = typer.Argument(..., help="Path to the skill directory"),
    action: Optional[str] = typer.Option(
        None,
        "--action",
        help="User decision to apply: allow | always_allow | block | rollback",
    ),
    version: Optional[str] = typer.Option(
        None,
        "--version",
        help="Target version for rollback decisions.",
    ),
    reason: Optional[str] = typer.Option(
        None,
        "--reason",
        help="Optional human-readable decision reason.",
    ),
    clear: bool = typer.Option(
        False,
        "--clear",
        help="Clear the latest version's user decision.",
    ),
) -> None:
    """Apply or clear a user decision for a skill version."""
    if clear and action is not None:
        typer.echo("Error: --clear and --action are mutually exclusive.", err=True)
        raise typer.Exit(code=1)
    result = invoke(
        "skill_ledger",
        command="decide",
        skill_dir=skill_dir,
        decision_action=action,
        target_version_id=version,
        reason=reason,
        clear=clear,
    )
    _forward(result)


# ---------------------------------------------------------------------------
# show
# ---------------------------------------------------------------------------


@app.command("show")
def cmd_show(
    skill_dir: str = typer.Argument(..., help="Path to the skill directory"),
    policy: Optional[str] = typer.Option(
        None,
        "--policy",
        callback=_validate_cli_policy,
        help="Activation policy to use when computing the active version.",
    ),
) -> None:
    """Show latest, active, decision, findings, and consistency details."""
    result = invoke(
        "skill_ledger",
        command="show",
        skill_dir=skill_dir,
        policy=policy,
    )
    _forward(result)


# ---------------------------------------------------------------------------
# export
# ---------------------------------------------------------------------------


@app.command("export")
def cmd_export(
    skill_dir: str = typer.Argument(..., help="Path to the skill directory"),
    version: str = typer.Option(
        "latest",
        "--version",
        help="Version to export: latest | active | v000001",
    ),
    output: str = typer.Option(
        ...,
        "--output",
        help="Directory that will receive snapshot/, manifest.json, and findings.json.",
    ),
    policy: Optional[str] = typer.Option(
        None,
        "--policy",
        callback=_validate_cli_policy,
        help="Activation policy to use when --version active is requested.",
    ),
) -> None:
    """Export an otherwise hidden signed snapshot for user review."""
    result = invoke(
        "skill_ledger",
        command="export",
        skill_dir=skill_dir,
        version=version,
        output=output,
        policy=policy,
    )
    _forward(result)


# ---------------------------------------------------------------------------
# rotate-keys (reserved stub)
# ---------------------------------------------------------------------------


@app.command("rotate-keys")
def cmd_rotate_keys() -> None:
    """Report that signing-key rotation is not implemented.

    This reserved command makes no key changes and exits non-zero when invoked.
    """
    typer.echo(
        "Error: rotate-keys is not implemented; no keys were changed.",
        err=True,
    )
    raise typer.Exit(code=1)


# ---------------------------------------------------------------------------
# Main entry (for direct module invocation: python -m ...)
# ---------------------------------------------------------------------------


def main() -> None:
    """Main entry point for the ``skill-ledger`` CLI."""
    app()


if __name__ == "__main__":
    main()
