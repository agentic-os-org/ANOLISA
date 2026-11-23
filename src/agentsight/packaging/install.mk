# =============================================================================
# INSTALL
# =============================================================================

INSTALL_PROFILE ?= system
DESTDIR ?=
ifeq ($(INSTALL_PROFILE),user)
PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
INSTALL_SYSTEMD ?= 0
else
PREFIX ?= /usr
BINDIR ?= /usr/local/bin
INSTALL_SYSTEMD ?= 1
endif
SYSCONFDIR ?= /etc
LIBDIR ?= $(PREFIX)/lib
SYSTEMD_SYSTEM_DIR ?= $(LIBDIR)/systemd/system
SERVICE_BINDIR ?= $(BINDIR)
SETCAP ?= auto
AGENTSIGHT_BIN ?= target/release/agentsight
AGENTSIGHT_CONFIG_DIR ?= $(SYSCONFDIR)/agentsight
START_BINARY ?= agentsight
START_CONFIG ?=

.PHONY: install-legacy install-prebuilt uninstall
install-legacy: build-frontend build-binary ## Build and install using the legacy profile and capabilities

install-legacy install-prebuilt:
	@enforcer_bin="$(ENFORCER_BIN)"; \
		if [ -z "$$enforcer_bin" ]; then \
			enforcer_bin=$$($(ENFORCER_BUILD)) || exit $$?; \
		fi; \
		install -d -m 0755 "$(DESTDIR)$(BINDIR)" && \
		python3 scripts/copy-enforcer.py "$$enforcer_bin" "$(DESTDIR)$(BINDIR)/agentsight-enforcer"
	install -d -m 0755 $(DESTDIR)$(AGENTSIGHT_CONFIG_DIR)
	install -p -m 0755 "$(AGENTSIGHT_BIN)" "$(DESTDIR)$(BINDIR)/agentsight"
	install -p -m 0644 agentsight.json $(DESTDIR)$(AGENTSIGHT_CONFIG_DIR)/config.json
	@if [ "$(SETCAP)" = "1" ] || { [ "$(SETCAP)" = "auto" ] && [ "$(INSTALL_PROFILE)" = "system" ] && [ -z "$(DESTDIR)" ] && command -v setcap >/dev/null 2>&1; }; then \
		setcap cap_bpf,cap_perfmon=ep $(DESTDIR)$(BINDIR)/agentsight; \
	else \
		echo "Skipping setcap for $(DESTDIR)$(BINDIR)/agentsight"; \
	fi
	@if [ "$(INSTALL_SYSTEMD)" = "1" ]; then \
		install -d -m 0755 $(DESTDIR)$(SYSTEMD_SYSTEM_DIR); \
		python3 packaging/systemd/render.py --bindir "$(SERVICE_BINDIR)" \
			$(if $(START_CONFIG),--config "$(START_CONFIG)") \
			--binary "$(START_BINARY)" --launcher "$(DESTDIR)$(BINDIR)/agentsight-start" \
			--unitdir "$(DESTDIR)$(SYSTEMD_SYSTEM_DIR)"; \
	fi

uninstall:
	rm -f $(foreach path,$(DELIVERY_UNINSTALL_FILES),"$(DESTDIR)$(path)")
	@for path in $(foreach path,$(DELIVERY_UNINSTALL_DIRS),"$(DESTDIR)$(path)"); do \
		rmdir "$$path" 2>/dev/null || true; \
	done
