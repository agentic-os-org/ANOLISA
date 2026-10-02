"""Default daemon background job registration."""

from agent_sec_cli.daemon.jobs.base import JobManager
from agent_sec_cli.daemon.jobs.skill_ledger import (
    SkillLedgerActivationJob,
)


def register_default_jobs(job_manager: JobManager) -> None:
    """Register daemon jobs that should start with every daemon instance.

    Concrete jobs live in this package as separate modules. Keep this file as
    the central startup registry so daemon startup order stays explicit.

    Prompt scanning needs no startup job: the native scanner compiles its
    rules on first use and reaches its L2/L4 models over HTTP, so there is
    nothing to preload or track.
    """
    job_manager.register(SkillLedgerActivationJob())
