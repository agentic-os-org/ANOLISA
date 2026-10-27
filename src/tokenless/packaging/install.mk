# Included legacy installation implementation; public entrypoint is ../Makefile.
INSTALL_PROFILE ?= user
DESTDIR ?=
AWPRJ ?= anolisa
NAMESPACE_DIR = $(if $(strip $(AWPRJ)),$(strip $(AWPRJ))/)

ifeq ($(INSTALL_PROFILE),user)
PREFIX ?= $(HOME)/.local
COSH_EXTENSION_DIR ?= $(HOME)/.copilot-shell/extensions/tokenless
else
PREFIX ?= /usr
COSH_EXTENSION_DIR ?= $(if $(strip $(AWPRJ)),$(PREFIX)/share/$(strip $(AWPRJ))/extensions/tokenless,$(PREFIX)/share/tokenless/cosh-extension)
endif

BINDIR ?= $(PREFIX)/bin
LIBEXECDIR ?= $(PREFIX)/libexec/$(NAMESPACE_DIR)tokenless
DATADIR ?= $(PREFIX)/share
SHARE_DIR ?= $(if $(strip $(AWPRJ)),$(DATADIR)/$(strip $(AWPRJ))/adapters/tokenless,$(DATADIR)/tokenless/adapters)
BIN_DIR ?= $(BINDIR)
LIB_DIR ?= $(LIBEXECDIR)
# Install binaries + adapter resources per FHS spec.
.PHONY: install-legacy
install-legacy: build install-binaries install-helpers install-adapter-resources install-cosh-extension

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
	bash packaging/payload.sh adapters "$(ADAPTER_SRC_DIR)" "$(DESTDIR)$(SHARE_DIR)" legacy
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
	bash packaging/payload.sh extension "$(ADAPTER_SRC_DIR)" "$(DESTDIR)$(COSH_EXTENSION_DIR)" legacy

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/tokenless
	rm -f $(DESTDIR)$(LIBEXECDIR)/rtk
	$(clean-stale-toon)
	rm -rf $(DESTDIR)$(SHARE_DIR)
	rm -rf $(DESTDIR)$(COSH_EXTENSION_DIR)
