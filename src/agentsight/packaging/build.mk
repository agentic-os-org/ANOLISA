# =============================================================================
# BUILD
# =============================================================================

LIBBPF_SYS_LIBRARY_PATH ?= $(shell pkg-config --variable=libdir libbpf 2>/dev/null || echo /usr/lib64:/usr/lib)
NPM_REGISTRY ?= https://registry.npmmirror.com
NPM_REPLACE_REGISTRY_HOST ?= always

.PHONY: build-binary
build-binary: ## Build agentsight binary
	env -u DESTDIR -u MAKEFLAGS -u MFLAGS -u MAKEOVERRIDES \
		LIBBPF_SYS_LIBRARY_PATH="$(LIBBPF_SYS_LIBRARY_PATH)" \
		cargo build --release

ENFORCER_BUILD = env -u DESTDIR -u MAKEFLAGS -u MFLAGS -u MAKEOVERRIDES \
	LIBBPF_SYS_LIBRARY_PATH="$(LIBBPF_SYS_LIBRARY_PATH)" ./scripts/build-enforcer.sh

.PHONY: build-enforcer
build-enforcer: ## Build the privileged daemon; print its private verified path
	@enforcer_bin=$$($(ENFORCER_BUILD)) || exit $$?; \
		install -d -m 0755 target/delivery && \
		python3 scripts/copy-enforcer.py "$$enforcer_bin" target/delivery/agentsight-enforcer && \
		install -p -m 0644 "$$enforcer_bin.sha256" target/delivery/agentsight-enforcer.sha256 || exit $$?; \
		printf '%s\n' "$$enforcer_bin"

.PHONY: build-frontend
build-frontend: ## Build and embed frontend into frontend-dist/
	cd dashboard && npm install \
		--registry="$(NPM_REGISTRY)" \
		--replace-registry-host="$(NPM_REPLACE_REGISTRY_HOST)" \
		&& npm run build:embed

.PHONY: build-all
build-all: build-frontend ## Build frontend, AgentSight, and enforcer
	$(MAKE) build-binary build-enforcer

.PHONY: build-dsh-plugin
build-dsh-plugin: ## Build the optional DSH plugin resources
	cd dsh-plugin && npm install && npm run build

.PHONY: build-mac
build-mac: build-frontend ## Build macOS local viewer (serve only; no eBPF)
	cargo build --release --bin agentsight

.PHONY: example
example: build-binary ## Build C FFI example (requires libagentsight)
	gcc -o target/release/agentsight_example examples/agentsight_example.c \
		-Iinclude -Ltarget/release -lagentsight -lpthread -ldl -lm

# The public aggregate records one explicit, verified install input; installers
# never search random enforcer build directories or trigger another build.
.PHONY: build
build: build-all build-dsh-plugin ## Build every deliverable and record the attested enforcer handoff
