# Single-source version sync: Cargo.toml is the authority. Every derived
# JSON file (adapter manifest, npm package, npm lockfile, MCP server
# descriptor, openclaw plugin manifest) is rewritten to match. The
# openclaw plugin's runtime version (sent in MCP initialize.clientInfo
# .version) is injected by esbuild's --define at bundle time using
# `npm_package_version` set by npm, which itself was just synced from
# $(VERSION) here.
#
# Idempotent: each file is only rewritten when its current `.version`
# differs from $(VERSION); a no-op invocation leaves mtime untouched so
# `make build` doesn't dirty the git tree on every run.

# Derive publication metadata from the component Cargo version.
SYNC_JSON_FILES := \
	$(ADAPTER_SRC_DIR)/manifest.json \
	$(OPENCLAW_PLUGIN_SRC_DIR)/package.json \
	$(OPENCLAW_PLUGIN_SRC_DIR)/openclaw.plugin.json \
	config/mcp-server.json

sync-versions: ## Sync $(VERSION) (from Cargo.toml) into all derived JSON files
	@command -v jq >/dev/null || { echo "ERROR: jq is required for sync-versions"; exit 1; }
	@# Auto-clean stray *.tmp on shell error (jq failure, disk full, SIGINT).
	@trap 'rm -f $(SYNC_JSON_FILES:%=%.tmp) $(OPENCLAW_PLUGIN_SRC_DIR)/package-lock.json.tmp' EXIT; \
	changed=0; \
	for f in $(SYNC_JSON_FILES); do \
		cur=$$(jq -r '.version // empty' "$$f" 2>/dev/null || echo ""); \
		if [ "$$cur" != "$(VERSION)" ]; then \
			jq --arg v "$(VERSION)" '.version = $$v' "$$f" > "$$f.tmp" && mv "$$f.tmp" "$$f" || exit 1; \
			echo "    $$f: $$cur -> $(VERSION)"; \
			changed=1; \
		fi; \
	done; \
	plock=$(OPENCLAW_PLUGIN_SRC_DIR)/package-lock.json; \
	cur=$$(jq -r '.version // empty' "$$plock" 2>/dev/null || echo ""); \
	if [ "$$cur" != "$(VERSION)" ]; then \
		jq --arg v "$(VERSION)" '.version = $$v | (.packages[""] | select(.) | .version) |= $$v' \
			"$$plock" > "$$plock.tmp" && mv "$$plock.tmp" "$$plock" || exit 1; \
		echo "    $$plock: $$cur -> $(VERSION)"; \
		changed=1; \
	fi; \
	if [ $$changed -eq 1 ]; then \
		echo "==> Synced derived files to $(VERSION)"; \
	fi

$(COMPONENT_CONTRACT): $(COMPONENT_CONTRACT_TEMPLATE) Cargo.toml
	@echo "==> Generating component contract (version=$(VERSION))..."
	@mkdir -p "$(@D)"
	@sed 's/@VERSION@/$(VERSION)/g' "$<" > "$@"

generate-component-contract: $(COMPONENT_CONTRACT)
