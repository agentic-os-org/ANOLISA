# Included npm packaging implementation; public entrypoint is ../Makefile.
NATIVE_NPM_TARGET = $(shell node -p 'process.platform + "-" + process.arch')
NATIVE_NPM_INPUT_DIR = $(CURDIR)/target/npm-prebuilt/$(NATIVE_NPM_TARGET)
# --- npm Packaging ---
# Preserve the convenient native entry point while keeping package-npm.js a
# pure validator/assembler.
prepare-npm-native-binaries: build-tokenless
	@install -d -m 0755 "$(NATIVE_NPM_INPUT_DIR)"
	@install -p -m 0755 \
		target/release/tokenless \
		third_party/rtk/target/release/rtk \
		"$(NATIVE_NPM_INPUT_DIR)/"

# Build and package the current host.
# The package this target produces ships scripts/postinstall.js, and in CI
# `make npm-package` is the step of the tokenless job that runs after
# actions/setup-node — the one place a supported Node is guaranteed. Gating
# packaging on the install-method suite is what keeps the postinstall ownership
# test executing in CI, in strict mode, so an interpreter that cannot load the
# script fails the build instead of reporting a skip.
npm-package: prepare-npm-native-binaries \
	test-install-script test-install-docs test-npm-postinstall-strict
	@echo "==> Packaging anolisa-tokenless for npm (current platform)..."
	node packaging/npm/package-npm.js

# Package all five prebuilt target directories from target/npm-prebuilt.
npm-package-all:
	@echo "==> Packaging anolisa-tokenless for npm (all platforms)..."
	node packaging/npm/package-npm.js --all

# Publish all packages to npm registry
# IMPORTANT: Platform packages must be published BEFORE the root package,
# because the root package declares them as optionalDependencies.
# If the root package is published first and a platform package fails,
# npm will have a package referencing non-existent dependencies.
#
# The registry is pinned explicitly: the nested npm/dist/* package roots do
# NOT inherit npm/.npmrc, so without --registry a user-level registry
# override could silently win. Already-published name@version pairs are
# skipped, so a partially failed publish run can simply be re-executed
# (npm forbids republishing the same version).
NPM_REGISTRY ?= https://registry.npmjs.org/

npm-publish: npm-package-all
	@echo "==> Publishing anolisa-tokenless packages to $(NPM_REGISTRY) (platform packages first, root last)..."
	@set -e; \
	for pkg in tokenless-linux-x64 tokenless-linux-arm64 tokenless-darwin-x64 tokenless-darwin-arm64 tokenless-win32-x64 tokenless; do \
		dir=npm/dist/$$pkg; \
		name=$$(node -p "require('./$$dir/package.json').name"); \
		ver=$$(node -p "require('./$$dir/package.json').version"); \
		if npm view "$$name@$$ver" version --registry=$(NPM_REGISTRY) >/dev/null 2>&1; then \
			echo "  $$name@$$ver already published — skipping"; \
		else \
			echo "  publishing $$name@$$ver..."; \
			(cd $$dir && npm publish --access public --registry=$(NPM_REGISTRY)); \
		fi; \
	done
