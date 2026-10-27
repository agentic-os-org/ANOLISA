# Source archive and RPM delivery implementation.
# RPM build paths
RPMBUILD_DIR ?= $(HOME)/rpmbuild
SPEC_FILE := $(NAME).spec


# =============================================================================
# DISTRIBUTION
# =============================================================================

dist: clean build-openclaw-plugin ## Create source + vendor tarballs for RPM build
	$(MAKE) generate-component-contract
	@echo "==> Vendoring crates..."
	cargo vendor vendor/
	@mkdir -p .cargo
	@printf '[source.crates-io]\nreplace-with = "vendored-sources"\n\n[source.vendored-sources]\ndirectory = "vendor"\n' > .cargo/config.toml
	@echo "==> Creating $(NAME)-$(VERSION).tar.gz..."
	@mkdir -p dist
	# Top-level dir is $(NAME)-$(VERSION) so it matches the spec's
	# `%setup -n %{name}-%{version}` and the CI archive shape.
	#
	# `examples/` belongs in the copy list because Cargo.toml declares
	# `[[example]] mcp-harness` with an explicit path, and cargo refuses
	# to build a declared target whose file is missing — an archive
	# without it breaks `cargo test` and `cargo build --examples` inside
	# the unpacked source. The spec's own `%build` runs a plain
	# `cargo build --release`, which does not select examples, so rpmbuild
	# cannot catch that omission.
	#
	# `docs/` is deliberately not copied: the component manuals moved to
	# the repository-level docs/user-guide tree (a46aea8d) and the spec
	# packages CHANGELOG.md only. Listing the removed directory made
	# `cp -R` exit non-zero, so this target failed outright.
	@rm -rf dist/$(NAME)-$(VERSION) && mkdir -p dist/$(NAME)-$(VERSION)
	@cp -R Cargo.toml Cargo.lock src examples config tests \
		Makefile CHANGELOG.md LICENSE \
		.gitignore packaging dist/$(NAME)-$(VERSION)/
	install -m 0644 "$(COMPONENT_CONTRACT)" "dist/$(NAME)-$(VERSION)/$(COMPONENT_CONTRACT_EXPORT)"
	@mkdir -p dist/$(NAME)-$(VERSION)/.cargo
	@cp .cargo/config.toml dist/$(NAME)-$(VERSION)/.cargo/config.toml
	# Adapter: include source + pre-built dist/index.js, exclude
	# node_modules/tests so the source tarball stays under a few MB.
	@mkdir -p dist/$(NAME)-$(VERSION)/$(ADAPTER_SRC_DIR)
	tar -cf - -C $(ADAPTER_SRC_DIR) \
		--exclude='node_modules' \
		--exclude='.tsbuildinfo' \
		--exclude='tests' \
		. | tar -xf - -C dist/$(NAME)-$(VERSION)/$(ADAPTER_SRC_DIR)/
	# Fail here rather than inside rpmbuild: every path the spec's %build
	# and %install read, plus every target path Cargo.toml declares, must
	# be in the staged tree. A copy list that silently rots is what broke
	# this target the first time.
	@set -e; \
	staged="dist/$(NAME)-$(VERSION)"; \
	for f in src/main.rs src/lib.rs examples/mcp_harness.rs \
			config/default.toml config/mcp-server.json \
			packaging/systemd/anolisa-memory@.service \
			packaging/systemd/anolisa-memory-tmpfiles.conf \
			$(ADAPTER_SRC_DIR)/manifest.json \
			$(OPENCLAW_PLUGIN_SRC_DIR)/dist/index.js \
			$(OPENCLAW_PLUGIN_SRC_DIR)/openclaw.plugin.json \
			$(OPENCLAW_PLUGIN_SRC_DIR)/package.json \
			$(OPENCLAW_PLUGIN_SRC_DIR)/scripts/detect.sh \
			$(OPENCLAW_PLUGIN_SRC_DIR)/scripts/install.sh \
			$(OPENCLAW_PLUGIN_SRC_DIR)/scripts/uninstall.sh \
			$(COMPONENT_CONTRACT_EXPORT) CHANGELOG.md LICENSE; do \
		test -e "$$staged/$$f" || { \
			echo "ERROR: $$f is missing from $$staged but packaging/rpm/agent-memory.spec.in or Cargo.toml requires it" >&2; \
			exit 1; \
		}; \
	done
	tar czf dist/$(NAME)-$(VERSION).tar.gz -C dist $(NAME)-$(VERSION)
	@echo "==> Creating $(NAME)-$(VERSION)-vendor.tar.gz..."
	# Top-level dir is `vendor/` (no $(NAME)-vendor wrapper) so that
	# `%setup -a 1` lands vendor/ next to the unpacked sources, where
	# cargo `--offline` can find it.
	tar czf dist/$(NAME)-$(VERSION)-vendor.tar.gz vendor
	@rm -rf dist/$(NAME)-$(VERSION) vendor
	@echo "==> Tarballs: dist/$(NAME)-$(VERSION).tar.gz + dist/$(NAME)-$(VERSION)-vendor.tar.gz"

# =============================================================================
# RPM BUILD
# =============================================================================

spec: ## Generate RPM spec from template (substitute @VERSION@)
	@echo "==> Generating $(SPEC_FILE) with version $(VERSION)..."
	sed 's/@VERSION@/$(VERSION)/g' packaging/rpm/$(NAME).spec.in > $(SPEC_FILE)

rpm: dist spec ## Build RPM package
	@echo "==> Building RPM for $(NAME)-$(VERSION)..."
	mkdir -p $(RPMBUILD_DIR)/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
	cp dist/$(NAME)-$(VERSION).tar.gz $(RPMBUILD_DIR)/SOURCES/
	cp dist/$(NAME)-$(VERSION)-vendor.tar.gz $(RPMBUILD_DIR)/SOURCES/
	cp $(SPEC_FILE) $(RPMBUILD_DIR)/SPECS/
	rpmbuild -bb --define "_topdir $(RPMBUILD_DIR)" $(RPMBUILD_DIR)/SPECS/$(SPEC_FILE)
	@echo "==> RPM built. Check $(RPMBUILD_DIR)/RPMS/"

srpm: dist spec ## Build source RPM
	@echo "==> Building SRPM..."
	mkdir -p $(RPMBUILD_DIR)/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
	cp dist/$(NAME)-$(VERSION).tar.gz $(RPMBUILD_DIR)/SOURCES/
	cp dist/$(NAME)-$(VERSION)-vendor.tar.gz $(RPMBUILD_DIR)/SOURCES/
	cp $(SPEC_FILE) $(RPMBUILD_DIR)/SPECS/
	rpmbuild -bs --define "_topdir $(RPMBUILD_DIR)" $(RPMBUILD_DIR)/SPECS/$(SPEC_FILE)
	@echo "==> SRPM built. Check $(RPMBUILD_DIR)/SRPMS/"

# =============================================================================
# UTILITY
