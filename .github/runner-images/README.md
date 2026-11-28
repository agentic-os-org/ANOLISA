# Test container images

To register a Linux amd64 test image, add a directory with a Dockerfile here
and add one entry to `catalog.json`. For example, the entry below registers
`python-tests/Dockerfile`.

```json
{"alias": "python-tests", "context": "python-tests"}
```

The context is a direct child directory. Use a unique alias of 2–63 lowercase
letters, digits and single hyphens. Fork PRs build changed recipes. Merging to
`main` publishes each changed image to its own private repository. Read access
for the alias is refreshed from the merged catalog. A manual main-branch run
rebuilds every entry.

Copy the pinned image reference from the publish job summary into the test
job's `container.image`. The `ci-images.invalid/<alias>:sha256-<hex>` form is
resolved by the managed test runners. A newly added alias becomes usable
after publication and reader refresh; a fork can build a new recipe before
that point. Removing an entry stops renewal of its pull grant. Previously
issued short-lived grants expire and the published artifacts are retained.
