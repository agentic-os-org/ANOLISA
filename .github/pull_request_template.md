## Why

<!-- What problem does this change solve? Why is it needed now? -->

## What changed

<!-- Keep this focused on one logical change. -->

## Related issue

<!-- Use `closes #123`, `fixes #123`, or `resolves #123`.
For a genuinely trivial change, use `no-issue: <brief reason>`. -->

## User / Agent impact

<!-- Describe visible behavior changes. Write "None" when there is no user-facing impact. -->

## Risk and compatibility

- [ ] Public CLI, API, configuration, or documented behavior changed
- [ ] Privileged or security-sensitive behavior changed
- [ ] Cross-component contract changed
- [ ] Migration or rollback guidance is needed

<!-- Explain any checked item, or write "Low risk" when none apply. -->

## Validation

<!-- List the relevant tests, commands, environments, or manual checks. -->

### AgentSight validation

<!-- Required for PRs touching src/agentsight; omit for other components.
GitHub binds each push to its current SHA and CI results automatically.
Use the existing Test agentsight check for build/test results; do not copy
its SHA, environment, commands or output into this description after each push.
Explain the regression case once; update it only when the tested behavior changes.
Describe any verification CI cannot cover (for example, real-kernel BPF tracing),
including environment, commands/results and gaps. Omit that detail if CI covers it. -->

- Regression evidence: <regression test/case; or not applicable with reason>

## Documentation and rollback

<!-- What documentation changed? How can this be reverted or disabled? -->
