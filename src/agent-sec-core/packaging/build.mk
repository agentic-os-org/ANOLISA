# =============================================================================
# BUILD
# =============================================================================

# Build output directory — all artifacts are collected here after build.
# Override from outside: make build-all BUILD_DIR=/path/to/output
BUILD_DIR ?= target
CLI_WHEEL_BUILD_DIR ?= agent-sec-cli/target/wheels
CLI_WHEEL_PATTERN := agent_sec_cli-*.whl
COMPONENT_CONTRACT ?= packaging/anolisa/component.toml

# The normal targets retain their network-enabled developer workflow. RPM builds
# select OFFLINE_BUILD through build-all-v2-offline after Source1 supplies every
# Cargo crate and npm package cache entry.
OFFLINE_BUILD ?= 0
CARGO_OFFLINE_FLAGS = $(if $(filter 1,$(OFFLINE_BUILD)),--offline,)
CARGO_BUILD_ENV = $(if $(filter 1,$(OFFLINE_BUILD)),CARGO_HOME="$(abspath $(BUILD_DIR)/cargo-home)",)
NPM_INSTALL = $(if $(filter 1,$(OFFLINE_BUILD)),npm ci --offline --include=dev --omit=optional --ignore-scripts --cache .npm-cache --no-audit --no-fund,npm install --include=dev)
RAW_RUST_BUILDER ?= cargo
RAW_RUST_TARGET ?=
RAW_RUST_RELEASE_SUBDIR = $(if $(RAW_RUST_TARGET),$(RAW_RUST_TARGET)/,)release

# Component targets shared by the V1 and V2 aggregates. Only the CLI/daemon
# build differs between them, so one list prevents the two aggregates from
# drifting apart as components are added. $(1) is the CLI slot; its position is
# preserved so prerequisite order stays identical to the pre-V2 build-all.
define component_build_targets
build-sandbox $(1) build-openclaw-plugin build-hermes-plugin build-qwen-code-extension stage-codex-plugin stage-qoder-plugin stage-cosh-extension stage-skills stage-component-manifest
endef

# Install targets shared by the V1 and V2 rpmbuild aggregates; the CLI install
# target is prepended by each aggregate.
RPM_INSTALL_COMMON_TARGETS := install-cosh-hook install-codex-plugin install-qoder-plugin \
	install-openclaw-plugin install-hermes-plugin install-qwen-code-extension \
	install-skills-files install-component-manifest

# V2 Rust binaries are collected here by build-cli-v2 and consumed by
# install-cli-v2, mirroring the build/install split used for linux-sandbox.
V2_BIN_BUILD_DIR ?= $(BUILD_DIR)/v2/bin
V2_CARGO_TARGET_DIR ?= $(abspath v2/target)

define require_single_cli_wheel
set -- "$(1)"/$(CLI_WHEEL_PATTERN); \
if [ "$$#" -ne 1 ] || [ ! -f "$$1" ]; then \
	echo "ERROR: expected exactly one agent-sec-cli wheel in $(1)" >&2; \
	exit 1; \
fi;
endef

.PHONY: build-sandbox
build-sandbox: ## Build linux-sandbox binary
	cd linux-sandbox && $(CARGO_BUILD_ENV) $(RAW_RUST_BUILDER) build --release --locked $(CARGO_OFFLINE_FLAGS)
	install -d -m 0755 $(BUILD_DIR)
	cp -p linux-sandbox/target/$(RAW_RUST_RELEASE_SUBDIR)/linux-sandbox $(BUILD_DIR)/

.PHONY: build-cli
build-cli: ## Build agent-sec-cli wheel with maturin (Rust + Python)
	rm -f $(CLI_WHEEL_BUILD_DIR)/$(CLI_WHEEL_PATTERN)
	install -d -m 0755 $(BUILD_DIR)/wheels
	rm -f $(BUILD_DIR)/wheels/$(CLI_WHEEL_PATTERN)
	cd agent-sec-cli && uv sync --only-group dev --no-install-project && \
		uv run --no-sync maturin build --release -i python3.11 --manylinux off
	@$(call require_single_cli_wheel,$(CLI_WHEEL_BUILD_DIR)) \
		cp -p "$$1" $(BUILD_DIR)/wheels/

.PHONY: build-cli-v2
build-cli-v2: ## Build V2 Rust CLI and daemon binaries
	cd v2 && $(CARGO_BUILD_ENV) CARGO_TARGET_DIR="$(abspath $(V2_CARGO_TARGET_DIR))" $(RAW_RUST_BUILDER) build --release --locked $(CARGO_OFFLINE_FLAGS) --workspace
	install -d -m 0755 $(V2_BIN_BUILD_DIR)
	cp -p "$(abspath $(V2_CARGO_TARGET_DIR))/$(RAW_RUST_RELEASE_SUBDIR)/agent-sec-cli" \
		"$(abspath $(V2_CARGO_TARGET_DIR))/$(RAW_RUST_RELEASE_SUBDIR)/agent-sec-daemon" \
		"$(abspath $(V2_CARGO_TARGET_DIR))/$(RAW_RUST_RELEASE_SUBDIR)/agent-sec-daemon-health" \
		$(V2_BIN_BUILD_DIR)/

.PHONY: setup
setup: ## Install all dependencies (including dev), create .venv
	cd agent-sec-cli && uv sync

.PHONY: build-openclaw-plugin
build-openclaw-plugin: ## Build openclaw-plugin TypeScript sources
	cd openclaw-plugin && NODE_ENV=development $(NPM_INSTALL) && npm run build
	install -d -m 0755 $(BUILD_DIR)/openclaw-plugin/dist
	install -d -m 0755 $(BUILD_DIR)/openclaw-plugin/scripts
	cp openclaw-plugin/openclaw.plugin.json $(BUILD_DIR)/openclaw-plugin/
	cp openclaw-plugin/package.json $(BUILD_DIR)/openclaw-plugin/
	cp -r openclaw-plugin/dist/* $(BUILD_DIR)/openclaw-plugin/dist/
	cp -r openclaw-plugin/scripts/* $(BUILD_DIR)/openclaw-plugin/scripts/

.PHONY: build-hermes-plugin
build-hermes-plugin: ## Stage hermes-plugin Python sources to BUILD_DIR
	install -d -m 0755 $(BUILD_DIR)/hermes-plugin/src
	install -d -m 0755 $(BUILD_DIR)/hermes-plugin/scripts
	cp -rp hermes-plugin/src/. $(BUILD_DIR)/hermes-plugin/src/
	cp -rp hermes-plugin/scripts/. $(BUILD_DIR)/hermes-plugin/scripts/

.PHONY: build-qwen-code-extension
build-qwen-code-extension: ## Stage Qwen Code extension files (no compilation)
	install -d -m 0755 $(BUILD_DIR)/qwen-code-extension
	cp -rp qwen-code-extension/. $(BUILD_DIR)/qwen-code-extension/

.PHONY: stage-codex-plugin
stage-codex-plugin: ## Stage codex-plugin hooks and install script to BUILD_DIR
	install -d -m 0755 $(BUILD_DIR)/codex-plugin
	cp -rp codex-plugin/hooks-plugin $(BUILD_DIR)/codex-plugin/
	cp -p codex-plugin/install.sh $(BUILD_DIR)/codex-plugin/
	cp -rp codex-plugin/.agents $(BUILD_DIR)/codex-plugin/

.PHONY: stage-qoder-plugin
stage-qoder-plugin: ## Stage qoder-plugin hooks and install script to BUILD_DIR
	install -d -m 0755 $(BUILD_DIR)/qoder-plugin
	cp -rp qoder-plugin/. $(BUILD_DIR)/qoder-plugin/

.PHONY: stage-cosh-extension
stage-cosh-extension: ## Stage cosh-extension hooks to BUILD_DIR
	install -d -m 0755 $(BUILD_DIR)/cosh-extension
	cp -rp cosh-extension/. $(BUILD_DIR)/cosh-extension/

.PHONY: stage-skills
stage-skills: ## Stage skill files to BUILD_DIR
	install -d -m 0755 $(BUILD_DIR)/skills
	cp -rp skills/. $(BUILD_DIR)/skills/

.PHONY: stage-tools
stage-tools: ## Stage tools (sign-skill.sh) to BUILD_DIR
	install -d -m 0755 $(BUILD_DIR)/tools
	cp -p tools/sign-skill.sh $(BUILD_DIR)/tools/

.PHONY: stage-component-manifest
stage-component-manifest: ## Stage the selected component contract into BUILD_DIR
	install -d -m 0755 $(BUILD_DIR)/share/anolisa/components/sec-core
	install -p -m 0644 $(COMPONENT_CONTRACT) \
		$(BUILD_DIR)/share/anolisa/components/sec-core/component.toml

# The default build target matches the default install target: both provide
# the V2 Rust CLI/daemon. V1 RPM builds select build-all-v1 explicitly.
.PHONY: build
build: build-all-v2 ## Build all V2 deliverable artifacts

.PHONY: build-all
build-all: build-all-v2 ## Build all components with the V2 Rust CLI/daemon

.PHONY: build-all-v1
build-all-v1: $(call component_build_targets,build-cli) ## Build all components with the V1 Python CLI
	@echo "📦 All artifacts collected to $(BUILD_DIR)/"

.PHONY: build-all-v2
build-all-v2: $(call component_build_targets,build-cli-v2) ## Build all components with the V2 Rust CLI/daemon
	@echo "📦 All artifacts collected to $(BUILD_DIR)/"

.PHONY: build-all-v2-offline
build-all-v2-offline: OFFLINE_BUILD := 1
build-all-v2-offline: $(call component_build_targets,build-cli-v2) ## Build V2 components from vendored dependencies only
	@echo "📦 Offline V2 artifacts collected to $(BUILD_DIR)/"

.PHONY: build-all-source-v2
build-all-source-v2: build-all-v2 ## Build the V2 artifacts for source installation

.PHONY: export-requirements
export-requirements: ## Re-export agent-sec-cli/requirements.txt from uv.lock
	cd agent-sec-cli && uv export --frozen --no-dev --no-emit-project -o requirements.txt

.PHONY: download-deps
download-deps: ## Download ALL Python deps for agent-sec-cli (requires network)
	install -d -m 0755 $(BUILD_DIR)/wheels
	pip3 download --dest $(BUILD_DIR)/wheels/ --no-cache-dir \
		--python-version 3.11.6 --only-binary=:all: \
		--timeout 60 \
		--require-hashes \
		--index-url https://pypi.org/simple/ \
		-r agent-sec-cli/requirements.txt

.PHONY: stage-cli
stage-cli: ## Install all wheels to local staging dir (requires uv)
	install -d -m 0755 $(CLI_STAGED_SITE)
	uv pip install --target $(CLI_STAGED_SITE) --no-deps --no-cache --link-mode copy \
		$(BUILD_DIR)/wheels/*.whl
	rm -f $(CLI_STAGED_SITE)/.lock
