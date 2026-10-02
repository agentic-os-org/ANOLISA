# Asset Verification

[中文版](../../../zh/agent-security/agent-sec-core/asset-verification.md)

Asset Verification checks the distribution integrity of Skill directories. It verifies a
GPG-signed manifest, checks the SHA-256 digest of every manifest entry, and rejects additional
non-hidden regular files that the manifest does not cover.

This command validates release or deployment signatures. It is separate from
[Skill Ledger](skill-ledger.md), which uses Ed25519 signatures to maintain a local runtime history.

## Installation

```bash
# Recommended: install the standard ANOLISA raw component in system mode
sudo anolisa --install-mode system install sec-core

# Alternative for Alinux systems with the YUM repository configured
sudo yum install anolisa agent-sec-core
sudo anolisa --install-mode system adopt sec-core

# Source build for developers
./scripts/build-all.sh --component sec-core
```

Asset Verification requires GnuPG 2.0 or later. The component package includes the verifier
configuration and trusted public keys.

## Verify Skills

Run batch discovery with no option, or bypass discovery and verify one Skill explicitly:

```bash
# Scan the default installation roots
agent-sec-cli verify

# Verify exactly one Skill directory
agent-sec-cli verify --skill /path/to/skill
```

Each candidate must contain:

| Path | Purpose |
|------|---------|
| `.skill-meta/Manifest.json` | Lists the expected SHA-256 digest for each signed file |
| `.skill-meta/.skill.sig` | GPG detached signature for `Manifest.json` |

The verifier loads trusted `.asc` public keys from the packaged
`agent_sec_cli/asset_verify/trusted-keys/` directory. A missing manifest or signature, an untrusted
or invalid signature, a missing or modified manifest entry, or an additional unsigned regular file
fails that candidate.

## Default Discovery Roots

`agent-sec-cli verify` reads the packaged `asset_verify/config.conf`. The default configuration has
two optional discovery roots:

| Installation topology | Discovery root |
|-----------------------|----------------|
| RPM | `/usr/share/anolisa/skills` |
| Standard ANOLISA raw package | `/usr/local/share/anolisa/skills` |

Every immediate, non-hidden child directory is a candidate Skill. Discovery follows these rules:

- A missing root is skipped silently.
- An empty root, or one containing no immediate visible directories, contributes no candidates.
- Roots that resolve to the same canonical path are scanned once.
- An existing non-directory root or a root that cannot be enumerated is an operation error.
- An unreadable candidate, like a candidate with an invalid signature or hash, is a verification
  failure.

The two default roots are fixed package data. They are not rendered from an arbitrary installation
prefix. For a relocated or custom Skill, use `agent-sec-cli verify --skill /path/to/skill`.

## Outcomes and Exit Codes

Every completed run prints `CHECKED`, `PASSED`, and `FAILED` counts, followed by one exact final
status line:

| Outcome | Meaning | Final status line | Exit code |
|---------|---------|-------------------|-----------|
| `verified` | At least one candidate was checked and every candidate passed | `VERIFICATION PASSED` | `0` |
| `failed` | At least one candidate failed verification | `VERIFICATION FAILED` | `1` |
| `no_candidates` | Discovery completed normally but found no candidates | `VERIFICATION SKIPPED: NO CANDIDATE SKILLS` | `0` |

`no_candidates` is a successful best-effort discovery result. It does not mean that any asset was
verified. Missing and empty default roots therefore do not turn an installation with no Skills into
a verification failure.

`--skill` names exactly one candidate. A nonexistent path, non-directory path, unreadable Skill, or
invalid Skill is therefore always `failed` with exit code `1`; explicit verification never maps
that input to `no_candidates`.

Configuration parsing, trusted-key loading, canonicalization, and root-enumeration failures are
operation errors and exit `1`. Because no stable verification result exists in that case, telemetry
may omit the asset outcome. The CLI reports these operation errors on standard error.

For completed runs, telemetry records `seccore.asset_outcome` as `verified`, `failed`, or
`no_candidates`, together with passed and failed counts. Discovery roots and Skill paths are not
uploaded.

## Sign Skills in Self-Managed Deployments

Release packages should retain their release signatures. For a self-managed deployment, use the
source-tree signing helper to create a local signing key, export its public key to the verifier trust
directory, and sign Skills:

```bash
cd src/agent-sec-core
tools/sign-skill.sh --check
tools/sign-skill.sh --init
tools/sign-skill.sh /path/to/skill --force
```

Re-sign a Skill after changing any covered file. See the
[Skill Signing Guide](../../../../../src/agent-sec-core/tools/SIGNING_GUIDE.md) for key management,
batch signing, and CI/CD usage.

## Troubleshooting

- `no_candidates`: check whether Skills are installed below either default root, or pass `--skill`
  for a custom location.
- `ERR_MANIFEST_MISSING` or `ERR_SIG_MISSING`: the discovered directory is a candidate but is not
  signed with the Asset Verification format.
- `ERR_SIG_INVALID`: verify that the signing public key is installed in the packaged
  `trusted-keys/` directory and re-sign if the manifest changed.
- `ERR_HASH_MISMATCH` or `ERR_UNEXPECTED_FILE`: inspect the Skill contents, then restore the signed
  release or deliberately re-sign the reviewed content.
- An operation error before an outcome: check `config.conf`, the trusted-key directory, and the type
  and permissions of every existing discovery root.
