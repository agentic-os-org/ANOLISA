# Component Issue source analysis

Repository writers can analyze one Issue's title and body in
`agentic-os-org/ANOLISA` against a frozen source snapshot using its component policy.
Discussion comments are excluded from analysis. The workflow checks source
references and priority rules, performs same-agent review, and produces a report.
It does not execute the component, functional tests, or dynamic reproductions.

## Component policies

The shared engine is `.github/scripts/issue-analysis.js`. Component identity,
paths, labels and triagers come from `.github/components.json`; source entrypoints
and scope notes live in `.github/issue-analysis/components.json`. All public
components have a profile. Internal components are excluded.
The common policy is `policy.md`. Source-backed legacy V1 P3 classification is
restricted to SecCore; other components use the captured GitHub priority rules.
The existing SecCore workflow and script entrypoints delegate to this engine.

## Run the workflow manually

Configure `QODER_PERSONAL_ACCESS_TOKEN` as a repository Actions secret and define
`priority:p0` through `priority:p3` with descriptions in the repository's GitHub
label settings. Grading uses those descriptions captured at the start of the run.
Label colors do not affect grading. Missing descriptions leave priority unresolved.
The workflow must be available on the repository's default branch for manual
dispatch.

In Actions, select **AI / SecCore Issue Analysis**, then **Run workflow**:

- `issue_number`: the Issue number, without a URL or `#` prefix.
- `target_ref`: the source branch, tag, or full SHA; defaults to `main`.
- `publish`: defaults to `false`; set to `true` to authorize Issue writeback.

For example, replace `1234` with the Issue number to analyze:

```bash
gh workflow run sec-core-issue-analysis.yml \
  --repo agentic-os-org/ANOLISA --ref main \
  -f issue_number=1234 -f target_ref=main -f publish=false
```

`--ref` selects the workflow version; `target_ref` selects the source and is
resolved once to a full SHA. An Issue with any existing `priority:*` label is
skipped before source collection or model calls, for either value of `publish`.
The skipped run records its reason in `run.json`.

## Automatic analysis after Issue Router

Issue Router calls this workflow after applying a new `sec-core` routing
decision, using the routed Issue number, `target_ref=main`, and `publish=true`.
The original dispatcher must have repository write, maintain, or admin access.
Dry runs, shadow mode, low confidence, conflicting manual component labels,
and repeated decisions do not start analysis. Existing `priority:*` labels
still skip analysis before model calls. A later owner notification failure
does not block analysis of an applied route; cancelled runs do not start it.
Use **Re-run failed jobs** to retry failed analysis or publication rather than
dispatching the same routing decision again.

## Reports and publication

Active Issues confirmed by source to affect only legacy V1 receive `priority:p3`
and a comment explaining the V1-only backlog classification. This does not
verify the reported defect or its impact. Unknown version scope remains
unresolved and receives no priority; shared code affecting V2 is graded normally.

Reports and comments state that the finding is based on source analysis only;
no dynamic reproduction or functional tests were executed for that Issue.
Claims include a verification plan for checks to be performed separately.
Snapshots, validated results, source references, and bounded failure diagnostics
are retained in Actions artifacts for seven days.

Publication rechecks authorization, Issue content, state, and labels. Before
publishing an approved priority, it also checks that the GitHub priority
descriptions still match the snapshot; changed definitions require a fresh run.
Adding, editing, or deleting discussion comments does not invalidate the
analysis or block publication. The publisher preserves unrelated comments and
protects its selected previous report against overwriting intervening edits.
It verifies its report write and the Issue fields after publication.
Use **Re-run failed jobs** to retry publication with the existing validated
analysis artifact, including when its planned label was already added.

## Local checks

```bash
node --test .github/scripts/sec-core-issue-analysis.test.js
```

These regression tests validate the automation; they do not verify the behavior
reported in a real Issue. The model runs in an isolated, read-only source
environment with no GitHub credentials or shell tool. The separate publisher
receives no model credential.
