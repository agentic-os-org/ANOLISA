# Makefile for copilot-shell

.PHONY: help build test deps-ci clean install uninstall \
        deps bundle lint format preflight start debug rpm

# ── Variables ──
DESTDIR  ?=
INSTALL_PROFILE ?= system
ifeq ($(INSTALL_PROFILE),user)
PREFIX   ?= $(HOME)/.local
BINDIR   ?= $(PREFIX)/bin
else
PREFIX   ?= /usr
BINDIR   ?= $(PREFIX)/bin
endif
LIBDIR    = $(PREFIX)/lib/copilot-shell
DATADIR   = $(PREFIX)/share/copilot-shell
DOCDIR    = $(PREFIX)/share/doc/copilot-shell

# Platform detection for vendor binaries (maps to vendor/ripgrep/{ARCH}-{PLATFORM}/)
UNAME_S  := $(shell uname -s)
UNAME_M  := $(shell uname -m)
VENDOR_PLATFORM := $(if $(filter Linux,$(UNAME_S)),linux,$(if $(filter Darwin,$(UNAME_S)),darwin,))
VENDOR_ARCH     := $(if $(filter x86_64,$(UNAME_M)),x64,$(if $(filter aarch64 arm64,$(UNAME_M)),arm64,))
VENDOR_RG_DIR   := vendor/ripgrep/$(VENDOR_ARCH)-$(VENDOR_PLATFORM)

help:
	@echo "Usage: make <target>"
	@echo ""
	@echo "  build        Build dist/ for distribution (esbuild + prepare)"
	@echo "  test         Run the test suite"
	@echo "  deps-ci      Clean-install npm deps (CI / RPM build)"
	@echo "  clean        Remove generated files"
	@echo "  install      Install to PREFIX (supports DESTDIR for RPM)"
	@echo "  uninstall    Remove installed files"

# ══════════════════════════════════════════════════════
# Build & Test
# ══════════════════════════════════════════════════════

build:
	npm run bundle
	npm run prepare:package

test:
	npm run test

deps-ci:
	npm ci --ignore-scripts

clean:
	npm run clean

# ══════════════════════════════════════════════════════
# Install / Uninstall
# ══════════════════════════════════════════════════════
# RPM build:   make install DESTDIR=%{buildroot} INSTALL_PROFILE=system PREFIX=/usr
# User install: make build && make install INSTALL_PROFILE=user PREFIX=$(HOME)/.local

install: dist/package.json
	@echo "Installing copilot-shell to $(DESTDIR)$(LIBDIR) ..."
	# -- runtime code --
	install -d "$(DESTDIR)$(LIBDIR)"
	install -m 644 dist/cli.js "$(DESTDIR)$(LIBDIR)/cli.js"
	install -m 644 dist/package.json "$(DESTDIR)$(LIBDIR)/package.json"
	@if [ -d dist/node_modules ]; then \
		cp -pr dist/node_modules "$(DESTDIR)$(LIBDIR)/"; \
		echo "Pruning unused node-pty platform packages (keeping $(VENDOR_PLATFORM)-$(VENDOR_ARCH))..."; \
		for variant in darwin-arm64 darwin-x64 linux-arm64 linux-x64 win32-arm64 win32-x64; do \
			if [ "$$variant" != "$(VENDOR_PLATFORM)-$(VENDOR_ARCH)" ]; then \
				rm -rf "$(DESTDIR)$(LIBDIR)/node_modules/@lydell/node-pty-$$variant"; \
			fi; \
		done; \
	fi
	# -- arch-independent data --
	install -d "$(DESTDIR)$(DATADIR)/locales"
	cp -pr dist/locales/* "$(DESTDIR)$(DATADIR)/locales/"
	ln -snf "$(DATADIR)/locales" "$(DESTDIR)$(LIBDIR)/locales"
	@if [ -d hooks ]; then \
		install -d "$(DESTDIR)$(DATADIR)/hooks"; \
		cp -pr hooks/* "$(DESTDIR)$(DATADIR)/hooks/"; \
	fi
	@if [ -d dist/examples ]; then \
		install -d "$(DESTDIR)$(LIBDIR)/examples"; \
		cp -pr dist/examples/* "$(DESTDIR)$(LIBDIR)/examples/"; \
	fi
	# -- vendor binaries (current platform only) --
	@if [ -n "$(VENDOR_ARCH)" ] && [ -n "$(VENDOR_PLATFORM)" ] && [ -d "dist/$(VENDOR_RG_DIR)" ]; then \
		install -d "$(DESTDIR)$(LIBDIR)/$(VENDOR_RG_DIR)"; \
		install -m 755 dist/$(VENDOR_RG_DIR)/rg "$(DESTDIR)$(LIBDIR)/$(VENDOR_RG_DIR)/rg"; \
	fi
	@if [ -f "dist/vendor/ripgrep/COPYING" ]; then \
		install -d "$(DESTDIR)$(LIBDIR)/vendor/ripgrep"; \
		install -m 644 dist/vendor/ripgrep/COPYING "$(DESTDIR)$(LIBDIR)/vendor/ripgrep/COPYING"; \
	fi
	# -- documentation --
	install -d "$(DESTDIR)$(DOCDIR)"
	install -m 644 dist/README.md "$(DESTDIR)$(DOCDIR)/README.md"
	install -m 644 dist/LICENSE "$(DESTDIR)$(DOCDIR)/LICENSE"
	# -- executables --
	install -d "$(DESTDIR)$(BINDIR)"
	sed 's|@@LIBDIR@@|$(LIBDIR)|g' scripts/cosh-wrapper.sh > "$(DESTDIR)$(BINDIR)/cosh"
	chmod 755 "$(DESTDIR)$(BINDIR)/cosh"
	ln -sf cosh "$(DESTDIR)$(BINDIR)/co"
	ln -sf cosh "$(DESTDIR)$(BINDIR)/copilot"
	@echo "Installed: $(DESTDIR)$(BINDIR)/{cosh,co,copilot}"

uninstall:
	rm -f "$(DESTDIR)$(BINDIR)/cosh" "$(DESTDIR)$(BINDIR)/co" "$(DESTDIR)$(BINDIR)/copilot"
	rm -rf "$(DESTDIR)$(LIBDIR)"
	rm -rf "$(DESTDIR)$(DATADIR)"
	rm -rf "$(DESTDIR)$(DOCDIR)"
	@echo "Uninstalled copilot-shell from $(DESTDIR)$(PREFIX)"

# ══════════════════════════════════════════════════════
# Development helpers (forwarding to npm scripts)
# ══════════════════════════════════════════════════════

bundle: build  ## Alias for 'build'

deps:
	npm install

lint:
	npm run lint

format:
	npm run format

preflight:
	npm run preflight

start:
	npm run start

debug:
	npm run debug

rpm:
	npm run package:rpm
