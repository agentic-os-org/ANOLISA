# Token-Less Unified Build System
# Builds tokenless (schema/response compression, TOON encoding via the
# toon-format library) and rtk (command rewriting)

INSTALL_PROFILE ?= user
DESTDIR ?=

ifeq ($(INSTALL_PROFILE),user)
PREFIX ?= $(HOME)/.local
COSH_EXTENSION_DIR ?= $(HOME)/.copilot-shell/extensions/tokenless
else
PREFIX ?= /usr
COSH_EXTENSION_DIR ?= $(PREFIX)/share/anolisa/extensions/tokenless
endif

BINDIR ?= $(PREFIX)/bin
LIBEXECDIR ?= $(PREFIX)/libexec/anolisa/tokenless
DATADIR ?= $(PREFIX)/share
SHARE_DIR ?= $(DATADIR)/anolisa/adapters/tokenless
BIN_DIR ?= $(BINDIR)
LIB_DIR ?= $(LIBEXECDIR)
ADAPTER_SRC_DIR := adapters/tokenless
OPENCLAW_PLUGIN_SRC_DIR := $(ADAPTER_SRC_DIR)/openclaw
DSH_PLUGIN_SRC_DIR := $(ADAPTER_SRC_DIR)/dsh
ADAPTER_TEMPLATES := \
    $(ADAPTER_SRC_DIR)/manifest.json \
    $(ADAPTER_SRC_DIR)/openclaw/package.json \
    $(ADAPTER_SRC_DIR)/openclaw/openclaw.plugin.json \
    $(ADAPTER_SRC_DIR)/dsh/package.json \
    $(ADAPTER_SRC_DIR)/hermes/plugin.yaml \
    $(ADAPTER_SRC_DIR)/qoder/.qoder-plugin/plugin.json \
    $(ADAPTER_SRC_DIR)/claude-code/.claude-plugin/plugin.json \
    $(ADAPTER_SRC_DIR)/codex/.codex-plugin/plugin.json \
    $(ADAPTER_SRC_DIR)/qwencode/qwen-extension.json \
    $(ADAPTER_SRC_DIR)/qwenpaw/plugin.json \
    $(ADAPTER_SRC_DIR)/qwenpaw/requirements.txt
# Canonical RPM/raw component contract — kept separate from the adapter
# payload tree and stamped from the workspace version before packaging.
# Shipped to %{_datadir}/anolisa/components/tokenless/component.toml and
# embedded in raw archives as .anolisa/component.toml.
COMPONENT_CONTRACT := .anolisa/component.toml
VERSION := $(shell grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
NATIVE_NPM_TARGET := $(shell node -p 'process.platform + "-" + process.arch')
NATIVE_NPM_INPUT_DIR := $(CURDIR)/target/npm-prebuilt/$(NATIVE_NPM_TARGET)
PYTHON_RUNTIME_DIR := python/tokenless
AGENTSCOPE_PACKAGE_DIR := python/agentscope
PYTHON_WHEEL_DIR := $(CURDIR)/target/wheels
PYTHON_RTK_PACKAGE_DIR := $(CURDIR)/$(PYTHON_RUNTIME_DIR)/python/anolisa_tokenless/_bin
PYTHON_RTK_BINARY ?= $(CURDIR)/third_party/rtk/target/release/rtk
MATURIN ?= uvx --from 'maturin>=1.9,<2.0' maturin
PYPROJECT_BUILD ?= uvx --from 'build>=1,<2' pyproject-build

.PHONY: build build-tokenless build-openclaw-plugin build-dsh-plugin \
        stamp-adapter-templates stamp-python-packages generate-component-contract \
        install uninstall test lint clean package-raw stage-raw \
        test-raw-package test-npm-package test-toon-cleanup \
        install-binaries install-helpers install-adapter-resources install-cosh-extension \
        adapter-install adapter-uninstall adapter-scan \
        cosh-extension-install cosh-extension-uninstall \
        openclaw-install openclaw-uninstall \
        hermes-install hermes-uninstall \
        qoder-install qoder-uninstall \
        claude-code-install claude-code-uninstall \
        codex-install codex-uninstall \
        opencode-install opencode-uninstall \
        qwencode-install qwencode-uninstall \
        qwenpaw-install qwenpaw-uninstall \
        workbuddy-install workbuddy-uninstall \
        setup help \
        npm-package npm-package-all npm-publish prepare-npm-native-binaries \
        python-wheel test-python-runtime agentscope-wheel \
        test-agentscope-integration test-hooks test-claude-code-detect \
        test-hook-parity test-rtk-integration test-benchmark-rtk-probe

# Declared separately from the list above on purpose: that one is extended by
# other components' targets as well, and a shared line is a merge conflict
# waiting to happen.
.PHONY: test-install-methods test-install-script test-install-docs \
        test-npm-postinstall test-npm-postinstall-strict

# Default target
all: build

# Build both projects
build: build-tokenless build-openclaw-plugin build-dsh-plugin

build-tokenless:
	@echo "==> Building tokenless + rtk..."
	just setup-rtk
	cargo build --release
	cargo build --release --locked --manifest-path third_party/rtk/Cargo.toml

# Generate adapter config files from .in templates (VERSION from Cargo.toml).
stamp-adapter-templates: $(ADAPTER_TEMPLATES) $(COMPONENT_CONTRACT)

stamp-python-packages: $(AGENTSCOPE_PACKAGE_DIR)/pyproject.toml

generate-component-contract: $(COMPONENT_CONTRACT)

$(ADAPTER_SRC_DIR)/manifest.json: $(ADAPTER_SRC_DIR)/manifest.json.in Cargo.toml
	@echo "==> Generating manifest.json (version=$(VERSION))..."
	sed 's/@VERSION@/$(VERSION)/g' $< > $@

$(COMPONENT_CONTRACT): $(COMPONENT_CONTRACT).in Cargo.toml
	@echo "==> Generating component contract (version=$(VERSION))..."
	sed 's/@VERSION@/$(VERSION)/g' $< > $@


$(ADAPTER_SRC_DIR)/openclaw/package.json: $(ADAPTER_SRC_DIR)/openclaw/package.json.in Cargo.toml
	@echo "==> Generating openclaw/package.json (version=$(VERSION))..."
	sed 's/@VERSION@/$(VERSION)/g' $< > $@

$(ADAPTER_SRC_DIR)/openclaw/openclaw.plugin.json: $(ADAPTER_SRC_DIR)/openclaw/openclaw.plugin.json.in Cargo.toml
	@echo "==> Generating openclaw/openclaw.plugin.json (version=$(VERSION))..."
	sed 's/@VERSION@/$(VERSION)/g' $< > $@

$(ADAPTER_SRC_DIR)/dsh/package.json: $(ADAPTER_SRC_DIR)/dsh/package.json.in Cargo.toml
	@echo "==> Generating dsh/package.json (version=$(VERSION))..."
	sed 's/@VERSION@/$(VERSION)/g' $< > $@

$(ADAPTER_SRC_DIR)/hermes/plugin.yaml: $(ADAPTER_SRC_DIR)/hermes/plugin.yaml.in Cargo.toml
	@echo "==> Generating hermes/plugin.yaml (version=$(VERSION))..."
	sed 's/@VERSION@/$(VERSION)/g' $< > $@

$(ADAPTER_SRC_DIR)/qoder/.qoder-plugin/plugin.json: $(ADAPTER_SRC_DIR)/qoder/.qoder-plugin/plugin.json.in Cargo.toml
	@echo "==> Generating qoder/.qoder-plugin/plugin.json (version=$(VERSION))..."
	sed 's/@VERSION@/$(VERSION)/g' $< > $@

$(ADAPTER_SRC_DIR)/claude-code/.claude-plugin/plugin.json: \
    $(ADAPTER_SRC_DIR)/claude-code/.claude-plugin/plugin.json.in Cargo.toml
	@echo "==> Generating claude-code/.claude-plugin/plugin.json (version=$(VERSION))..."
	sed 's/@VERSION@/$(VERSION)/g' $< > $@

$(ADAPTER_SRC_DIR)/codex/.codex-plugin/plugin.json: $(ADAPTER_SRC_DIR)/codex/.codex-plugin/plugin.json.in Cargo.toml
	@echo "==> Generating codex/.codex-plugin/plugin.json (version=$(VERSION))..."
	sed 's/@VERSION@/$(VERSION)/g' $< > $@

$(ADAPTER_SRC_DIR)/qwencode/qwen-extension.json: \
    $(ADAPTER_SRC_DIR)/qwencode/qwen-extension.json.in Cargo.toml
	@echo "==> Generating qwencode/qwen-extension.json (version=$(VERSION))..."
	sed 's/@VERSION@/$(VERSION)/g' $< > $@

$(ADAPTER_SRC_DIR)/qwenpaw/plugin.json: $(ADAPTER_SRC_DIR)/qwenpaw/plugin.json.in Cargo.toml
	@echo "==> Generating qwenpaw/plugin.json (version=$(VERSION))..."
	sed 's/@VERSION@/$(VERSION)/g' $< > $@

$(ADAPTER_SRC_DIR)/qwenpaw/requirements.txt: $(ADAPTER_SRC_DIR)/qwenpaw/requirements.txt.in Cargo.toml
	@echo "==> Generating qwenpaw/requirements.txt (version=$(VERSION))..."
	sed 's/@VERSION@/$(VERSION)/g' $< > $@

$(AGENTSCOPE_PACKAGE_DIR)/pyproject.toml: \
    $(AGENTSCOPE_PACKAGE_DIR)/pyproject.toml.in Cargo.toml
	@echo "==> Generating agentscope/pyproject.toml (version=$(VERSION))..."
	sed 's/@VERSION@/$(VERSION)/g' $< > $@

# Build the tokenless OpenClaw plugin (TypeScript -> dist/index.js).
# Produces $(OPENCLAW_PLUGIN_SRC_DIR)/dist/index.js, which install-adapter-resources
# then copies into $(SHARE_DIR)/openclaw/dist/index.js. This file is referenced by
# the plugin's package.json ("main" / "openclaw.extensions") and must be present
# for `openclaw plugins install` to succeed.
build-openclaw-plugin: stamp-adapter-templates
	@echo "==> Building tokenless OpenClaw plugin..."
	cd $(OPENCLAW_PLUGIN_SRC_DIR) && npm ci --legacy-peer-deps --ignore-scripts --no-audit --no-fund
	cd $(OPENCLAW_PLUGIN_SRC_DIR) && npm run build
	@test -f $(OPENCLAW_PLUGIN_SRC_DIR)/dist/index.js \
	    || { echo "ERROR: $(OPENCLAW_PLUGIN_SRC_DIR)/dist/index.js was not produced"; exit 1; }

# The dsh entry is deliberately plain ESM and has no build-time dependency on
# the dsh SDK.  Keep this target as an explicit bundle validation seam so raw,
# RPM, and npm packaging all fail before shipping an incomplete native plugin.
build-dsh-plugin: stamp-adapter-templates
	@echo "==> Validating native dsh plugin bundle..."
	@test -f $(DSH_PLUGIN_SRC_DIR)/package.json \
	    || { echo "ERROR: $(DSH_PLUGIN_SRC_DIR)/package.json is missing"; exit 1; }
	@test -f $(DSH_PLUGIN_SRC_DIR)/cordis.patch.yml \
	    || { echo "ERROR: $(DSH_PLUGIN_SRC_DIR)/cordis.patch.yml is missing"; exit 1; }
	@test -f $(DSH_PLUGIN_SRC_DIR)/dist/index.js \
	    || { echo "ERROR: $(DSH_PLUGIN_SRC_DIR)/dist/index.js is missing"; exit 1; }
	@cd $(DSH_PLUGIN_SRC_DIR) && node --input-type=module -e "import('./dist/index.js').then((plugin) => { if (typeof plugin.apply !== 'function' || plugin.name !== 'anolisa-tokenless') process.exit(1) })"

# Install binaries + adapter resources per FHS spec.
install: build install-binaries install-helpers install-adapter-resources install-cosh-extension

install-binaries:
	@echo "==> Installing tokenless to $(DESTDIR)$(BINDIR)..."
	install -d -m 0755 $(DESTDIR)$(BINDIR)
	install -p -m 0755 target/release/tokenless $(DESTDIR)$(BINDIR)/

# Cleanup for the standalone toon binary shipped by older releases (TOON is
# now linked into tokenless via the toon-format library). $(LIBEXECDIR) is a
# Tokenless-owned directory, so its toon helper is always removed; the
# $(BINDIR)/toon entry is removed only when it is a symlink pointing into a
# Tokenless helper layout, so an unrelated user-installed `toon` executable
# is preserved.
define clean-stale-toon
rm -f $(DESTDIR)$(LIBEXECDIR)/toon
if [ -L "$(DESTDIR)$(BINDIR)/toon" ]; then \
    case "$$(readlink "$(DESTDIR)$(BINDIR)/toon")" in \
        */anolisa/tokenless/toon|*/anolisa/libexec/tokenless/toon) \
            rm -f "$(DESTDIR)$(BINDIR)/toon" ;; \
    esac; \
fi
endef

install-helpers:
	@echo "==> Installing helpers to $(DESTDIR)$(LIBEXECDIR)..."
	install -d -m 0755 $(DESTDIR)$(LIBEXECDIR)
	install -p -m 0755 third_party/rtk/target/release/rtk $(DESTDIR)$(LIBEXECDIR)/
	ln -sf $(LIBEXECDIR)/rtk $(DESTDIR)$(BINDIR)/rtk
	$(clean-stale-toon)

install-adapter-resources: build-openclaw-plugin build-dsh-plugin
	@echo "==> Installing adapter resources to $(DESTDIR)$(SHARE_DIR)..."
	rm -rf $(DESTDIR)$(SHARE_DIR)
	install -d -m 0755 $(DESTDIR)$(SHARE_DIR)
	cp -pr $(ADAPTER_SRC_DIR)/. $(DESTDIR)$(SHARE_DIR)/
	# Strip build-only artifacts from the installed plugin tree.
	rm -rf $(DESTDIR)$(SHARE_DIR)/agentscope
	rm -rf $(DESTDIR)$(SHARE_DIR)/openclaw/node_modules
	rm -f  $(DESTDIR)$(SHARE_DIR)/openclaw/package-lock.json
	find $(DESTDIR)$(SHARE_DIR) -name '*.in' -delete
	find $(DESTDIR)$(SHARE_DIR) -type f \( -name '*.py' -o -name '*.sh' \) -exec chmod 0755 {} +
	@test -f $(DESTDIR)$(SHARE_DIR)/openclaw/dist/index.js \
	    || { echo "ERROR: $(DESTDIR)$(SHARE_DIR)/openclaw/dist/index.js missing after install-adapter-resources"; exit 1; }
	@test -f $(DESTDIR)$(SHARE_DIR)/dsh/package.json \
	    || { echo "ERROR: $(DESTDIR)$(SHARE_DIR)/dsh/package.json missing after install-adapter-resources"; exit 1; }
	@test -f $(DESTDIR)$(SHARE_DIR)/dsh/cordis.patch.yml \
	    || { echo "ERROR: $(DESTDIR)$(SHARE_DIR)/dsh/cordis.patch.yml missing after install-adapter-resources"; exit 1; }
	@test -f $(DESTDIR)$(SHARE_DIR)/dsh/dist/index.js \
	    || { echo "ERROR: $(DESTDIR)$(SHARE_DIR)/dsh/dist/index.js missing after install-adapter-resources"; exit 1; }

install-cosh-extension:
	@echo "==> Installing tokenless cosh extension to $(DESTDIR)$(COSH_EXTENSION_DIR)..."
	rm -rf $(DESTDIR)$(COSH_EXTENSION_DIR)
	install -d -m 0755 $(DESTDIR)$(COSH_EXTENSION_DIR)/hooks $(DESTDIR)$(COSH_EXTENSION_DIR)/commands
	cp -pr $(ADAPTER_SRC_DIR)/common/hooks/. $(DESTDIR)$(COSH_EXTENSION_DIR)/hooks/
	cp -pr $(ADAPTER_SRC_DIR)/common/commands/. $(DESTDIR)$(COSH_EXTENSION_DIR)/commands/
	install -p -m 0644 $(ADAPTER_SRC_DIR)/common/cosh-extension.json $(DESTDIR)$(COSH_EXTENSION_DIR)/
	find $(DESTDIR)$(COSH_EXTENSION_DIR) -type f \( -name '*.py' -o -name '*.sh' \) -exec chmod 0755 {} +

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/tokenless
	rm -f $(DESTDIR)$(LIBEXECDIR)/rtk
	$(clean-stale-toon)
	rm -rf $(DESTDIR)$(SHARE_DIR)
	rm -rf $(DESTDIR)$(COSH_EXTENSION_DIR)

# Run tests
test: test-tokenless test-hooks test-integration test-rtk-integration test-hook-parity \
      test-adapters test-claude-code-detect test-raw-package \
      test-npm-package test-toon-cleanup test-benchmark-rtk-probe

test-tokenless:
	@echo "==> Testing tokenless..."
	cargo test --workspace -- --test-threads=1

# Declared separately from the .PHONY list at the top of this file on purpose:
# that list is extended by other components' targets as well, and a shared line
# is a merge conflict waiting to happen.
.PHONY: test-hook-scripts

# The two suites below are the only members of `test-hooks` that CI can run:
# tests/run-all-tests.sh drives the release binary installed on PATH on purpose
# (see the note above its test 5.0b), so `test-hooks` as a whole needs an
# installed tokenless and stays a developer-machine target. No workflow calls
# it -- grepping .github/ for test-hooks and for the two file names returns
# nothing -- so two contracts only ever ran on developer machines: run-hook.sh
# must resolve a hook from its own adapter tree before the FHS and
# home-directory fallbacks, and tool_ready_hook.sh must keep its hard bypass so
# the dormant legacy four-phase flow (which runs any readable env-fix script)
# stays unreachable. Both suites are plain bash over a mktemp sandbox with
# generated fixtures -- no cargo build output, node, jq, rtk, python3 or
# network, verified by running them under a PATH that holds only bash and
# coreutils -- so they cost well under a second. They ride on
# `test-integration` for the same reason test-claude-code-detect and
# test-toon-cleanup do: that is the hook-contract target the Test tokenless job
# already calls, so the pair reaches CI with no workflow edit and no new setup.
test-hook-scripts:
	@echo "==> Testing tokenless hook scripts (no external dependencies)..."
	bash tests/test-run-hook-install-scope.sh
	bash tests/test-tool-ready-readable-fixer.sh

test-hooks: test-hook-scripts
	@echo "==> Testing copilot-shell hooks..."
	bash tests/run-all-tests.sh

test-claude-code-detect:
	@echo "==> Testing claude-code detect.sh first-run retry..."
	bash tests/test-claude-code-detect-retry.sh

# Protocol v2 golden parity belongs to the integration contract, and this is the
# only tokenless test target a CI job calls (`make test-hook-parity` has no
# workflow caller). The prerequisite also builds target/debug/tokenless first:
# without that binary tests/test_hook_parity.py skips itself (skipUnless), so the
# goldens would go unenforced while the job still reported green. make runs the
# prerequisite once even though the aggregate `test` target lists both names.
#
# The shell suites ride along for the same reason: they are members of the
# `make test` aggregate that no workflow ever called (grepping .github/ for the
# targets and for the script names behind them returns nothing), so detect.sh's
# first-run plugin re-list retry, the stale-toon ownership rules in
# install-helpers/uninstall and the hook-script pair above only ran on developer
# machines. All of them are plain bash over a mktemp sandbox with stub binaries
# (the two older ones also recurse into make) -- no cargo, node or jq, and no
# rtk build -- so they cost about a second and are safe to run ahead of the
# Python contract below. They cannot
# go on `test-rtk-integration` instead: that target's own recipe is what fetches
# and builds the vendored rtk, and this one runs earlier in the CI job.
test-integration: test-hook-parity test-claude-code-detect test-toon-cleanup test-hook-scripts
	@echo "==> Testing hook integration (Python)..."
	python3 tests/test_compress_response_hook.py
	python3 tests/test_codex_response_diagnostics.py
	python3 tests/test_hermes_lifecycle.py
	python3 tests/test_compress_schema_hook.py
	python3 tests/test_hook_contract.py
	python3 tests/test_hermes_plugin_import.py
	python3 tests/test_agentscope_middleware.py
	python3 tests/test_agentscope_v1.py
	python3 tests/test_rewrite_hook.py
	python3 tests/test_cosh_extension_matcher.py
	python3 tests/test_resolve_agent_id.py
	python3 tests/test_release_regression_probe.py
	python3 tests/test_qwenpaw_plugin.py
	python3 tests/test_cosh_ng_compat.py

# The L1 runner records an `rtk_version` in benchmark_identity.json through a
# best-effort probe that must never stall the suite: a bare `$RTK_BIN` is
# resolved through PATH, the probe runs under a timeout(1) deadline whose child
# is reaped, and only a successful probe records a version (everything else
# collapses to "unavailable"). tests/test-benchmark-rtk-version-probe.sh guards
# exactly that contract with stub binaries, but no target ever ran it, so a
# probe regression reached CI unnoticed: the benchmark job would either stall on
# a hung rtk or silently publish an "unavailable" identity. The suite needs no
# rtk build and finishes in a few seconds, so it runs ahead of the pinned build
# below and fails fast. `test-rtk-integration` is the only RTK target a CI job
# calls ("Test bundled RTK integration"), which is what puts this in CI.
test-benchmark-rtk-probe:
	@echo "==> Testing benchmark RTK version probe..."
	bash tests/test-benchmark-rtk-version-probe.sh

test-rtk-integration: test-benchmark-rtk-probe
	bash scripts/setup-rtk.sh
	cargo build --release --locked --manifest-path third_party/rtk/Cargo.toml
	TOKENLESS_TEST_RTK_BINARY="$(CURDIR)/third_party/rtk/target/release/rtk" \
		python3 tests/test_rtk_integration.py

# Protocol v2 Common Hook parity: replay the contract corpus through the
# current hooks and real debug binary, then compare with the v2 goldens.
test-hook-parity:
	@echo "==> Testing hook golden parity (real binary)..."
	cargo build -p tokenless-cli
	python3 tests/test_hook_parity.py

# The npm channel's packaging contract suite is a member of the `test`
# aggregate that no workflow calls. The raw channel's sibling suite is invoked
# directly by the Tokenless prebuilt workflow, but grepping every file under
# .github/ for `test-npm-package` and for test-package-npm-prebuilt.sh returns
# nothing, so tests/test-package-npm-prebuilt.sh only ever ran on developer
# machines. (The one other uncovered member, test-hooks, stays out: its
# run-all-tests.sh deliberately drives an installed release binary on PATH, and
# no CI job installs tokenless.)
#
# The npm suite is the sole automated guard on npm/scripts/package-npm.js --
# the script that produces the tarballs `make npm-publish` pushes -- covering
# arch validation of the prebuilt binaries, the GLIBC 2.17 baseline warning,
# the --target selector aliases, exclusion of legacy agentscope build output,
# materialization of the claude-code and qwencode run-hook.sh dispatchers
# (`npm pack` silently drops symlinks, which would publish a plugin with no
# hooks), the rule that platform packages declare no `bin` (a collision makes
# npm remove every conflicting .bin link), the root package's
# optionalDependencies, and the dsh bundle contract. A regression in any of
# those ships broken installs behind a green build.
#
# It rides on this target because it needs node/npm and python3, and "Test
# adapter installers" is the first target the Test tokenless job calls after
# actions/setup-node -- test-integration runs earlier, while node is still
# unset. The pairing is also substantive rather than merely convenient: the
# packer rebuilds and bundles exactly the openclaw and dsh artifacts this
# target already builds, and the suite asserts they survive `npm pack` intact.
# It is deliberately not attached to `npm-package` instead: that target is the
# real packaging entry point whose npm/dist output the job's smoke test
# installs, and this suite fabricates its own prebuilt binaries and packs all
# four platforms, so running it there would leave fabricated tarballs beside
# the real ones. The suite backs up and restores any existing
# target/npm-prebuilt, needs no cargo build, and finishes in about 20 seconds.
#
# Under `make -j` the suite must not run beside the two plugin builds whose
# directories it rewrites through its own recursive make; test-npm-package
# declares them as prerequisites for exactly that reason.
test-adapters: build-openclaw-plugin build-dsh-plugin test-npm-package
	@echo "==> Testing OpenClaw adapter installer..."
	bash tests/test-openclaw-adapter-install.sh
	@echo "==> Testing openclaw Protocol v2 lifecycle..."
	node --test tests/test-openclaw-lifecycle-v2.mjs
	@echo "==> Testing native dsh adapter bundle..."
	bash tests/test-dsh-adapter.sh
	@echo "==> Testing qoder adapter installer..."
	bash tests/test-qoder-adapter-install.sh
	@echo "==> Testing OpenCode adapter..."
	bash tests/test-opencode-adapter-install.sh
	node tests/test-opencode-plugin.mjs
	@echo "==> Testing QwenPaw adapter installer..."
	bash tests/test-qwenpaw-adapter-install.sh
	@echo "==> Testing WorkBuddy adapter installer..."
	bash tests/test-workbuddy-adapter-install.sh

# Build the abi3 extension with unwind semantics so a Rust panic cannot abort
# the embedding Python interpreter. The wheel remains platform-specific, but
# one build supports CPython 3.11 and later on that platform.
python-wheel:
	@echo "==> Building Tokenless Python wheel..."
	@if [ "$(PYTHON_RTK_BINARY)" = "$(CURDIR)/third_party/rtk/target/release/rtk" ]; then \
		just setup-rtk; \
		cargo build --release --locked --manifest-path third_party/rtk/Cargo.toml; \
	fi
	@test -x "$(PYTHON_RTK_BINARY)" || { \
		echo "RTK binary is missing or not executable: $(PYTHON_RTK_BINARY)" >&2; \
		exit 1; \
	}
	install -d -m 0755 $(PYTHON_WHEEL_DIR)
	rm -f $(PYTHON_WHEEL_DIR)/anolisa_tokenless-*.whl
	@set -e; \
		install -d -m 0755 $(PYTHON_RTK_PACKAGE_DIR); \
		install -p -m 0755 $(PYTHON_RTK_BINARY) $(PYTHON_RTK_PACKAGE_DIR)/rtk; \
		trap 'rm -f $(PYTHON_RTK_PACKAGE_DIR)/rtk' EXIT; \
		cd $(PYTHON_RUNTIME_DIR); \
		$(MATURIN) build --profile python-release --interpreter python3 \
			--compatibility pypi --auditwheel check \
			--out $(PYTHON_WHEEL_DIR)

test-python-runtime: python-wheel
	@bash tests/test-python-runtime.sh

agentscope-wheel: stamp-python-packages
	@echo "==> Building Tokenless AgentScope integration wheel..."
	install -d -m 0755 $(PYTHON_WHEEL_DIR)
	rm -f $(PYTHON_WHEEL_DIR)/anolisa_tokenless_agentscope-*.whl
	$(PYPROJECT_BUILD) --wheel --outdir $(PYTHON_WHEEL_DIR) \
		$(AGENTSCOPE_PACKAGE_DIR)

test-agentscope-integration: python-wheel agentscope-wheel
	@bash tests/test-agentscope-integration.sh

# These targets only assemble the prebuilt BIN_DIR/{tokenless,rtk} files
# into the portable raw-package contract maintained by this component.
package-raw:
	@$(MAKE) -B stamp-adapter-templates
	@$(MAKE) build-openclaw-plugin build-dsh-plugin
	@./packaging/raw/package.sh package

stage-raw:
	@$(MAKE) -B stamp-adapter-templates
	@$(MAKE) build-openclaw-plugin build-dsh-plugin
	@./packaging/raw/package.sh stage

test-raw-package:
	@bash tests/test-package-raw.sh

# tests/test-package-npm-prebuilt.sh drives npm/scripts/package-npm.js, whose
# buildAdapters() recursively runs `make build-openclaw-plugin build-dsh-plugin`.
# Both are phony and always rebuild, and the openclaw step rewrites
# adapters/tokenless/openclaw/{node_modules,dist} (`npm ci`, then `npm run clean`
# + `tsc`). Left as a bare recipe, this target is free to run beside a sibling
# prerequisite that builds the same plugin, so one build's `tsc` reads
# node_modules/typescript/lib/*.d.ts while the other's `npm ci` is deleting it
# (TS6053 + TS2318, `make -j4 test-adapters` exits 2). Declaring the two builds
# as prerequisites serializes them: make builds them once, then runs the suite
# alone, and the recursive make just rebuilds the same artifacts. This ordering
# also protects the `test` aggregate, where the suite and test-adapters share a
# single deduplicated instance. Keep it if the suite is attached elsewhere.
test-npm-package: build-openclaw-plugin build-dsh-plugin
	@bash tests/test-package-npm-prebuilt.sh

test-toon-cleanup:
	@bash tests/test-toon-cleanup.sh

# Standalone curl installer/uninstaller behaviour plus the install-method docs
# that describe it. Offline: curl, npm and cargo are stubbed by the test.
# test-npm-postinstall runs the real npm/scripts/postinstall.js and skips when no
# interpreter can run it.
test-install-methods: test-npm-postinstall test-install-script test-install-docs

# Joined to the `test` aggregate by a second, prerequisite-only rule instead of by
# editing its own line: that line is extended by several components at once and is
# a recurring merge conflict, while GNU make unions the prerequisites of rules
# that carry no recipe.
test: test-install-methods

test-install-script:
	@echo "==> Testing standalone install/uninstall scripts..."
	@bash tests/test-install-script.sh

test-install-docs:
	@echo "==> Testing install-method documentation consistency..."
	@bash tests/test-install-docs.sh

# The npm package postinstall shares ~/.local/share/anolisa/adapters/tokenless
# with the anolisa CLI, so it has to identify the current owner before replacing
# the tree. Needs node; skips when no interpreter can run the script.
test-npm-postinstall:
	@echo "==> Testing npm postinstall adapter ownership..."
	@bash tests/test-npm-postinstall.sh

# Strict variant: an interpreter that cannot run postinstall.js is a failure, not
# a skip. This is the one npm-package depends on, because that target already
# needs a working node.
test-npm-postinstall-strict:
	@echo "==> Testing npm postinstall adapter ownership (a usable node is required)..."
	@TOKENLESS_REQUIRE_NODE=1 bash tests/test-npm-postinstall.sh

# Lint (rtk excluded — vendored third-party source)
lint:
	@echo "==> Linting tokenless..."
	cargo clippy --workspace -- -D warnings

# Format
fmt:
	@echo "==> Formatting..."
	cargo fmt --all

# Clean (rtk excluded from workspace — needs explicit clean)
clean:
	@echo "==> Cleaning..."
	cargo clean
	cargo clean --release --manifest-path third_party/rtk/Cargo.toml 2>/dev/null || true
	rm -rf $(OPENCLAW_PLUGIN_SRC_DIR)/dist $(OPENCLAW_PLUGIN_SRC_DIR)/node_modules
	rm -rf $(ADAPTER_SRC_DIR)/agentscope
	rm -f  $(ADAPTER_TEMPLATES) $(COMPONENT_CONTRACT)
	rm -f  $(AGENTSCOPE_PACKAGE_DIR)/pyproject.toml

# Create source tarball (excludes build artifacts).
# `clean` must finish before the generated contract and adapter manifests
# are re-stamped: as sibling prerequisites, `make -jN dist` may run the two
# concurrently and the cleanup can delete the stamped files, shipping an
# archive without the embedded install contract. Invoke the stamping
# sequentially from the recipe instead.
dist: clean
	$(MAKE) stamp-adapter-templates
	@echo "==> Creating tarball (with pre-patched rtk source)..."
	just setup-rtk
	tar czf ../tokenless-$(VERSION).tar.gz \
	    --exclude='target' \
	    --exclude='.git' \
	    -C .. tokenless
	@tar_list=$$(tar -tzf ../tokenless-$(VERSION).tar.gz); \
	for required in \
	    tokenless/.anolisa/component.toml \
	    tokenless/adapters/tokenless/manifest.json; do \
		echo "$$tar_list" | grep -q "^$$required$$" || { \
			echo "ERROR: tokenless-$(VERSION).tar.gz is missing $$required" >&2; \
			exit 1; \
		}; \
	done
	@echo "==> Tarball: ../tokenless-$(VERSION).tar.gz"

# Adapter management — invokes detect/install/uninstall action scripts per the
# ANOLISA FHS adapter spec.  Environment variables point to installed FHS paths.
ADAPTER_ENV = ANOLISA_PREFIX=$(HOME)/.local \
              ANOLISA_ADAPTER_DIR=$(SHARE_DIR) \
              ANOLISA_COMPONENT=tokenless \
              ANOLISA_VERSION=$(VERSION)

adapter-install: cosh-extension-install openclaw-install hermes-install qoder-install claude-code-install codex-install opencode-install qwencode-install qwenpaw-install workbuddy-install

adapter-uninstall: cosh-extension-uninstall openclaw-uninstall hermes-uninstall qoder-uninstall claude-code-uninstall codex-uninstall opencode-uninstall qwencode-uninstall qwenpaw-uninstall workbuddy-uninstall

adapter-scan:
	@echo "=== Tokenless Adapter Manifest ==="
	@python3 -c "import json; m=json.load(open('$(SHARE_DIR)/manifest.json')); print(f\"  component: {m['component']} v{m['version']}\"); [print(f\"  {t:12s} {c['compatibleVersions']:12s} \" + (', '.join(f'{len(v)} {k}' for k,v in c.get('capabilities',{}).items()) or 'no caps')) for t,c in m['targets'].items()]"

# --- Copilot Shell (cosh) — extension format ---
# cosh uses cosh-extension.json (not adapter scripts) for auto-discovery.
# The extension directory deploys hooks/commands/spec from adapter resources.

cosh-extension-install:
	@echo "==> Installing tokenless cosh extension..."
	@rm -rf $(COSH_EXTENSION_DIR)
	@install -d -m 0755 $(COSH_EXTENSION_DIR)/hooks $(COSH_EXTENSION_DIR)/commands
	cp -pr $(SHARE_DIR)/common/hooks/. $(COSH_EXTENSION_DIR)/hooks/
	cp -pr $(SHARE_DIR)/common/commands/. $(COSH_EXTENSION_DIR)/commands/
	install -p -m 0644 $(SHARE_DIR)/common/cosh-extension.json $(COSH_EXTENSION_DIR)/
	find $(COSH_EXTENSION_DIR) -type f \( -name '*.py' -o -name '*.sh' \) -exec chmod 0755 {} +
	@echo "==> tokenless cosh extension installed to $(COSH_EXTENSION_DIR)"

cosh-extension-uninstall:
	@echo "==> Uninstalling tokenless cosh extension..."
	rm -rf $(COSH_EXTENSION_DIR)
	@echo "==> tokenless cosh extension removed from $(COSH_EXTENSION_DIR)"

# --- OpenClaw ---

openclaw-install:
	@echo "==> Installing tokenless OpenClaw plugin..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=openclaw bash $(SHARE_DIR)/openclaw/scripts/detect.sh
	@$(ADAPTER_ENV) ANOLISA_TARGET=openclaw bash $(SHARE_DIR)/openclaw/scripts/install.sh

openclaw-uninstall:
	@echo "==> Uninstalling tokenless OpenClaw plugin..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=openclaw bash $(SHARE_DIR)/openclaw/scripts/uninstall.sh

# --- Hermes Agent ---
HERMES_ADAPTER_DIR = $(SHARE_DIR)/hermes

hermes-install:
	@echo "==> Installing Hermes Agent plugin..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=hermes bash $(HERMES_ADAPTER_DIR)/scripts/detect.sh || true
	@$(ADAPTER_ENV) ANOLISA_TARGET=hermes bash $(HERMES_ADAPTER_DIR)/scripts/install.sh

hermes-uninstall:
	@echo "==> Uninstalling Hermes Agent plugin..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=hermes bash $(HERMES_ADAPTER_DIR)/scripts/uninstall.sh

# --- Qoder CLI ---
QODER_ADAPTER_DIR = $(SHARE_DIR)/qoder

qoder-install:
	@echo "==> Installing tokenless Qoder CLI plugin..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=qoder bash $(QODER_ADAPTER_DIR)/scripts/detect.sh || true
	@$(ADAPTER_ENV) ANOLISA_TARGET=qoder bash $(QODER_ADAPTER_DIR)/scripts/install.sh

qoder-uninstall:
	@echo "==> Uninstalling tokenless Qoder CLI plugin..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=qoder bash $(QODER_ADAPTER_DIR)/scripts/uninstall.sh

# --- Claude Code ---
# Plugin is sourced from SHARE_DIR (FHS-installed adapter resources). When
# claude is not available the scripts gracefully no-op so `make adapter-install`
# stays usable on machines without Claude Code.
CLAUDE_CODE_ADAPTER_DIR = $(SHARE_DIR)/claude-code

claude-code-install:
	@echo "==> Installing Claude Code plugin..."
	@# detect.sh exits 1 (installable) or 2 (missing prereq); we tolerate both
	@# and let install.sh make the final call (it gracefully no-ops without claude CLI).
	@# This pre-install probe runs on a steady-state machine and its output is
	@# informational only, so skip detect.sh's first-run settling retries and
	@# the plugin registry re-lists that ride out the GH #3082 index lag.
	@$(ADAPTER_ENV) ANOLISA_TARGET=claude-code TOKENLESS_DETECT_RETRIES=0 TOKENLESS_DETECT_PLUGIN_RELISTS=0 bash $(CLAUDE_CODE_ADAPTER_DIR)/scripts/detect.sh || true
	@$(ADAPTER_ENV) ANOLISA_TARGET=claude-code bash $(CLAUDE_CODE_ADAPTER_DIR)/scripts/install.sh

claude-code-uninstall:
	@echo "==> Uninstalling Claude Code plugin..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=claude-code bash $(CLAUDE_CODE_ADAPTER_DIR)/scripts/uninstall.sh

# --- Codex ---
CODEX_ADAPTER_DIR = $(SHARE_DIR)/codex

codex-install:
	@echo "==> Installing Codex plugin..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=codex bash $(CODEX_ADAPTER_DIR)/scripts/detect.sh || true
	@$(ADAPTER_ENV) ANOLISA_TARGET=codex bash $(CODEX_ADAPTER_DIR)/scripts/install.sh

codex-uninstall:
	@echo "==> Uninstalling Codex plugin..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=codex bash $(CODEX_ADAPTER_DIR)/scripts/uninstall.sh

# --- OpenCode ---
OPENCODE_ADAPTER_DIR = $(SHARE_DIR)/opencode

opencode-install:
	@echo "==> Installing OpenCode plugin..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=opencode bash $(OPENCODE_ADAPTER_DIR)/scripts/detect.sh || true
	@$(ADAPTER_ENV) ANOLISA_TARGET=opencode bash $(OPENCODE_ADAPTER_DIR)/scripts/install.sh

opencode-uninstall:
	@echo "==> Uninstalling OpenCode plugin..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=opencode bash $(OPENCODE_ADAPTER_DIR)/scripts/uninstall.sh

# --- Qwen Code ---
QWENCODE_ADAPTER_DIR = $(SHARE_DIR)/qwencode

qwencode-install:
	@echo "==> Installing Qwen Code plugin..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=qwencode bash $(QWENCODE_ADAPTER_DIR)/scripts/detect.sh || true
	@$(ADAPTER_ENV) ANOLISA_TARGET=qwencode bash $(QWENCODE_ADAPTER_DIR)/scripts/install.sh

qwencode-uninstall:
	@echo "==> Uninstalling Qwen Code plugin..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=qwencode bash $(QWENCODE_ADAPTER_DIR)/scripts/uninstall.sh

# --- QwenPaw ---
QWENPAW_ADAPTER_DIR = $(SHARE_DIR)/qwenpaw

qwenpaw-install:
	@echo "==> Installing QwenPaw plugin..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=qwenpaw bash $(QWENPAW_ADAPTER_DIR)/scripts/detect.sh || true
	@$(ADAPTER_ENV) ANOLISA_TARGET=qwenpaw bash $(QWENPAW_ADAPTER_DIR)/scripts/install.sh

qwenpaw-uninstall:
	@echo "==> Uninstalling QwenPaw plugin..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=qwenpaw bash $(QWENPAW_ADAPTER_DIR)/scripts/uninstall.sh

# --- WorkBuddy (Tencent CodeBuddy) ---
# WorkBuddy hosts (CodeBuddy CLI, WorkBuddy IDE/Enterprise) share the
# ~/.codebuddy/settings.json hook protocol: install.sh merges the tokenless
# hook groups into that file. The scripts gracefully no-op when the
# .codebuddy home does not exist.
WORKBUDDY_ADAPTER_DIR = $(SHARE_DIR)/workbuddy

workbuddy-install:
	@echo "==> Installing tokenless WorkBuddy hooks..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=workbuddy bash $(WORKBUDDY_ADAPTER_DIR)/scripts/detect.sh || true
	@$(ADAPTER_ENV) ANOLISA_TARGET=workbuddy bash $(WORKBUDDY_ADAPTER_DIR)/scripts/install.sh

workbuddy-uninstall:
	@echo "==> Uninstalling tokenless WorkBuddy hooks..."
	@$(ADAPTER_ENV) ANOLISA_TARGET=workbuddy bash $(WORKBUDDY_ADAPTER_DIR)/scripts/uninstall.sh

# One-step setup: build + install + all adapters
setup: install adapter-install
	@echo ""
	@echo "============================================"
	@echo "  Token-Less setup complete!"
	@echo "============================================"
	@echo "  Binaries (FHS):"
	@echo "    tokenless -> $(BIN_DIR)/tokenless"
	@echo "    rtk       -> $(LIB_DIR)/rtk"
	@echo "  Extension (cosh):"
	@echo "    $(COSH_EXTENSION_DIR)/"
	@echo "  Adapter (FHS):"
	@echo "    $(SHARE_DIR)/"
	@echo "  Registered:"
	@echo "    cosh, openclaw, hermes, qoder, claude-code, codex, opencode, qwencode, qwenpaw, workbuddy"
	@echo ""
	@echo "Verify:"
	@echo "  tokenless --version"
	@echo "  rtk --version"
	@echo "  make adapter-scan"

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
	node npm/scripts/package-npm.js

# Package all four prebuilt target directories from target/npm-prebuilt.
npm-package-all:
	@echo "==> Packaging anolisa-tokenless for npm (all platforms)..."
	node npm/scripts/package-npm.js --all

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
	for pkg in tokenless-linux-x64 tokenless-linux-arm64 tokenless-darwin-x64 tokenless-darwin-arm64 tokenless; do \
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

# Help
help:
	@echo "Token-Less Build System"
	@echo ""
	@echo "Targets:"
	@echo "  build             Build tokenless + rtk + openclaw plugin"
	@echo "  build-tokenless   Build tokenless + rtk (via justfile)"
	@echo "  build-openclaw-plugin  Compile OpenClaw plugin TS -> dist/index.js"
	@echo "  install           Install binaries + adapter to FHS paths"
	@echo "  uninstall         Remove installed files"
	@echo "  test              Run all tests"
	@echo "  test-rtk-integration  Build pinned RTK and test its Tokenless patches"
	@echo "  test-benchmark-rtk-probe  Test the L1 runner's bounded rtk --version probe"
	@echo "  test-hook-scripts  Test the dependency-free hook bash suites (run in CI)"
	@echo "  lint              Run clippy checks"
	@echo "  fmt               Format code"
	@echo "  clean             Clean build artifacts"
	@echo "  python-wheel      Build the native anolisa-tokenless wheel"
	@echo "  agentscope-wheel  Build the AgentScope integration wheel"
	@echo "  test-agentscope-integration  Test the integration with supported AgentScope versions"
	@echo "  adapter-scan      List registered adapter capabilities"
	@echo "  adapter-install   Register all adapters (cosh+openclaw+hermes+qoder+claude-code+codex+opencode+qwencode+qwenpaw+workbuddy)"
	@echo "  adapter-uninstall Unregister all adapters"
	@echo "  cosh-extension-install  Install cosh extension"
	@echo "  cosh-extension-uninstall Uninstall cosh extension"
	@echo "  openclaw-install  Register OpenClaw plugin"
	@echo "  openclaw-uninstall Unregister OpenClaw plugin"
	@echo "  hermes-install    Install Hermes Agent plugin"
	@echo "  hermes-uninstall  Uninstall Hermes Agent plugin"
	@echo "  qoder-install     Register Qoder CLI plugin"
	@echo "  qoder-uninstall   Unregister Qoder CLI plugin"
	@echo "  claude-code-install   Register Claude Code plugin via claude CLI"
	@echo "  claude-code-uninstall Unregister Claude Code plugin"
	@echo "  codex-install     Register Codex plugin via codex CLI"
	@echo "  codex-uninstall   Unregister Codex plugin"
	@echo "  opencode-install  Register OpenCode local plugin"
	@echo "  opencode-uninstall Unregister OpenCode local plugin"
	@echo "  qwencode-install  Register Qwen Code plugin via qwen CLI"
	@echo "  qwencode-uninstall Unregister Qwen Code plugin"
	@echo "  qwenpaw-install   Install QwenPaw plugin via qwenpaw CLI"
	@echo "  qwenpaw-uninstall Uninstall QwenPaw plugin"
	@echo "  workbuddy-install Merge tokenless hooks into WorkBuddy settings.json"
	@echo "  workbuddy-uninstall Remove tokenless hooks from WorkBuddy settings.json"
	@echo "  setup             Full setup: build + install + register adapters"
	@echo "  npm-package       Build and package the current host"
	@echo "  npm-package-all   Package target/npm-prebuilt target directories"
	@echo "  npm-publish       Build and publish all npm packages"
	@echo "  package-raw       Package prebuilt BIN_DIR binaries for one raw target"
	@echo "  stage-raw         Stage one raw payload into DESTDIR"
	@echo "  help              Show this help"
	@echo ""
	@echo "Variables:"
	@echo "  INSTALL_PROFILE  user or system (default: user)"
	@echo "  DESTDIR          Staging root for package builds"
	@echo "  PREFIX           Install prefix (default: ~/.local or /usr)"
	@echo "  BINDIR           Binary install path"
	@echo "  LIBEXECDIR       Helper binary install path"
	@echo "  SHARE_DIR        Adapter resources path"
	@echo "  COSH_EXTENSION_DIR  Cosh extension path"
	@echo "  BIN_DIR          Directory containing prebuilt tokenless and rtk"
	@echo "  TARGET_OS        Raw target OS: linux or macos"
	@echo "  TARGET_ARCH      Raw target arch: x86_64 or aarch64"
	@echo "  OUTPUT_DIR       Raw artifact output directory (default: target/raw)"
