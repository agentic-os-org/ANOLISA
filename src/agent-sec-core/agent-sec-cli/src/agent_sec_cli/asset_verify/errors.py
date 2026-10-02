"""Skill verification error definitions."""


class SkillVerifyError(Exception):
    """Base exception for skill verification"""

    code = 1


class ErrSigMissing(SkillVerifyError):
    code = 10

    def __init__(self, skill_name: str) -> None:
        super().__init__(
            f"ERR_SIG_MISSING: Missing .skill-meta/.skill.sig in '{skill_name}'"
        )


class ErrManifestMissing(SkillVerifyError):
    code = 11

    def __init__(self, skill_name: str) -> None:
        super().__init__(
            f"ERR_MANIFEST_MISSING: Missing .skill-meta/Manifest.json in '{skill_name}'"
        )


class ErrSigInvalid(SkillVerifyError):
    code = 12

    def __init__(self, skill_name: str, detail: str = "") -> None:
        super().__init__(
            f"ERR_SIG_INVALID: Signature verification failed for '{skill_name}'. {detail}"
        )


class ErrHashMismatch(SkillVerifyError):
    code = 13

    def __init__(
        self, skill_name: str, file_path: str, expected: str, actual: str
    ) -> None:
        super().__init__(
            f"ERR_HASH_MISMATCH: File hash mismatch for '{file_path}' in '{skill_name}'\n"
            f"  Expected: {expected}\n"
            f"  Actual  : {actual}"
        )


class ErrUnexpectedFile(SkillVerifyError):
    code = 14

    def __init__(self, skill_name: str, file_path: str) -> None:
        super().__init__(
            f"ERR_UNEXPECTED_FILE: Unsigned file '{file_path}' present in '{skill_name}'"
        )


class ErrConfigMissing(SkillVerifyError):
    code = 20

    def __init__(self, path: str) -> None:
        super().__init__(f"ERR_CONFIG_MISSING: Config file not found: {path}")


class ErrConfigInvalid(SkillVerifyError):
    code = 22

    def __init__(self, path: str, detail: str) -> None:
        super().__init__(f"ERR_CONFIG_INVALID: Invalid config file '{path}': {detail}")


class ErrNoTrustedKeys(SkillVerifyError):
    code = 21

    def __init__(self, path: str) -> None:
        super().__init__(f"ERR_NO_TRUSTED_KEYS: No public keys found in '{path}'")
