---
name: github
version: 1.0.0
description: "Interact with GitHub using the `gh` CLI. Use `gh issue`, `gh pr`, `gh run`, and `gh api` for issues, PRs, CI runs, and advanced queries."
layer: application
lifecycle: usage
---
# GitHub Skill

Use the `gh` CLI to interact with GitHub. Always specify `--repo owner/repo` when not in a git directory, or use URLs directly.

## Install

In Alinux4, use cmd as follow:

```bash
# 1. 导入 GitHub CLI 官方仓库
curl -o /etc/yum.repos.d/gh-cli.repo https://cli.github.com/packages/rpm/gh-cli.repo

# 2. 安装 gh (可能需要sudo权限)
dnf install gh -y

# 3. 验证安装
gh --version
```

## Pull Requests

Check CI status on a PR:

```bash
gh pr checks 55 --repo owner/repo
```

List recent workflow runs:

```bash
gh run list --repo owner/repo --limit 10
```

View a run and see which steps failed:

```bash
gh run view <run-id> --repo owner/repo
```

View logs for failed steps only:

```bash
gh run view <run-id> --repo owner/repo --log-failed
```

## API for Advanced Queries

The `gh api` command is useful for accessing data not available through other subcommands.

Get PR with specific fields:

```bash
gh api repos/owner/repo/pulls/55 --jq '.title, .state, .user.login'
```

## JSON Output

Most commands support `--json` for structured output.  You can use `--jq` to filter:

```bash
gh issue list --repo owner/repo --json number,title --jq '.[] | "\(.number): \(.title)"'
```
