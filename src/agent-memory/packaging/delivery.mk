# Included by the component root Makefile; no second public Makefile/profile.
.PHONY: test-rpm-packages
test-packaging: test-rpm-packages
test-rpm-packages:
	python3 tests/test_rpm_packages.py

DELIVERY_ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/..)
# File-origin legacy install defaults must not leak into the public file-only installer.
delivery-input = $(if $(filter command line environment override,$(origin $(1))),$($(1)),$(2))
# Installation profiles affect delivered files, never runtime config/data paths.
DELIVERY_PROFILE := $(call delivery-input,PROFILE,standalone)
DELIVERY_INSTALL_PROFILE := $(call delivery-input,INSTALL_PROFILE,$(if $(filter 0,$(shell id -u)),system,user))
DELIVERY_AWPRJ := $(call delivery-input,AWPRJ,$(if $(filter legacy-anolisa,$(DELIVERY_PROFILE)),anolisa,))
ifeq ($(DELIVERY_INSTALL_PROFILE),user)
DELIVERY_PREFIX := $(call delivery-input,PREFIX,$(HOME)/.local)
DELIVERY_DATA_ROOT := $(if $(strip $(XDG_DATA_HOME)),$(XDG_DATA_HOME),$(HOME)/.local/share)
else
DELIVERY_PREFIX := $(call delivery-input,PREFIX,/usr/local)
DELIVERY_DATA_ROOT := $(DELIVERY_PREFIX)/share
endif
DELIVERY_BINDIR := $(call delivery-input,BINDIR,$(DELIVERY_PREFIX)/bin)
DELIVERY_LIBDIR := $(call delivery-input,LIBDIR,$(DELIVERY_PREFIX)/lib)
DELIVERY_LIBEXECDIR := $(call delivery-input,LIBEXECDIR,$(DELIVERY_PREFIX)/libexec/$(if $(strip $(DELIVERY_AWPRJ)),$(DELIVERY_AWPRJ)/)agent-memory)
ifeq ($(DELIVERY_INSTALL_PROFILE),user)
DELIVERY_UNITDIR := $(call delivery-input,UNITDIR,$(DELIVERY_DATA_ROOT)/systemd/user)
else
DELIVERY_UNITDIR := $(call delivery-input,UNITDIR,$(DELIVERY_LIBDIR)/systemd/user)
endif
DELIVERY_DOCDIR := $(call delivery-input,DOCDIR,$(DELIVERY_DATA_ROOT)/doc/agent-memory)
DELIVERY_BINARY := $(call delivery-input,BINARY,$(DELIVERY_ROOT)/target/release/agent-memory)

ifeq ($(DELIVERY_PROFILE),legacy-anolisa)
DELIVERY_DATADIR := $(call delivery-input,DATADIR,$(DELIVERY_DATA_ROOT)/anolisa)
DELIVERY_ADAPTERDIR := $(call delivery-input,ADAPTERDIR,$(DELIVERY_DATADIR)/adapters/agent-memory)
DELIVERY_CONFIGDIR := $(call delivery-input,CONFIGDIR,$(DELIVERY_DATADIR)/agent-memory)
DELIVERY_MCPDIR := $(call delivery-input,MCPDIR,$(DELIVERY_DATADIR)/mcp-servers)
else
DELIVERY_DATADIR := $(call delivery-input,DATADIR,$(DELIVERY_DATA_ROOT)/$(if $(strip $(DELIVERY_AWPRJ)),$(DELIVERY_AWPRJ)/)agent-memory)
ifneq ($(strip $(DELIVERY_AWPRJ)),)
DELIVERY_ADAPTERDIR := $(call delivery-input,ADAPTERDIR,$(if $(filter $(DELIVERY_DATA_ROOT)/$(DELIVERY_AWPRJ)/agent-memory,$(patsubst %/,%,$(DELIVERY_DATADIR))),$(DELIVERY_DATA_ROOT)/$(DELIVERY_AWPRJ)/adapters/agent-memory,$(DELIVERY_DATADIR)/adapters))
else
ifeq ($(DELIVERY_INSTALL_PROFILE),user)
DELIVERY_ADAPTERDIR := $(call delivery-input,ADAPTERDIR,$(DELIVERY_DATADIR)/adapters)
else
DELIVERY_ADAPTERDIR := $(call delivery-input,ADAPTERDIR,$(DELIVERY_LIBEXECDIR)/adapters)
endif
endif
DELIVERY_CONFIGDIR := $(call delivery-input,CONFIGDIR,$(DELIVERY_DATADIR))
DELIVERY_MCPDIR := $(call delivery-input,MCPDIR,$(DELIVERY_DATADIR)/mcp-servers)
endif

DELIVERY_INSTALL_ARGS = $(call delivery-quote,INSTALL_PROFILE=$(DELIVERY_INSTALL_PROFILE)) $(call delivery-quote,PROFILE=$(DELIVERY_PROFILE)) \
	$(call delivery-quote,PREFIX=$(DELIVERY_PREFIX)) $(call delivery-quote,AWPRJ=$(DELIVERY_AWPRJ)) \
	$(call delivery-quote,DESTDIR=$(DESTDIR)) $(call delivery-quote,BINARY=$(DELIVERY_BINARY)) \
	$(call delivery-quote,LIBDIR=$(DELIVERY_LIBDIR)) $(call delivery-quote,LIBEXECDIR=$(DELIVERY_LIBEXECDIR)) $(call delivery-quote,ADAPTERDIR=$(DELIVERY_ADAPTERDIR)) \
	$(call delivery-quote,BINDIR=$(DELIVERY_BINDIR)) $(call delivery-quote,DATADIR=$(DELIVERY_DATADIR)) $(call delivery-quote,SHARE_DIR=$(DELIVERY_ADAPTERDIR)) \
	$(call delivery-quote,CONFIGDIR=$(DELIVERY_CONFIGDIR)) $(call delivery-quote,MCPDIR=$(DELIVERY_MCPDIR)) $(call delivery-quote,UNITDIR=$(DELIVERY_UNITDIR)) $(call delivery-quote,DOCDIR=$(DELIVERY_DOCDIR))

delivery-quote = '$(subst ','"'"',$(1))'

# Uninstall the same paths as the public installer for the selected layout.
uninstall: BINDIR := $(DELIVERY_BINDIR)
uninstall: CONFIGDIR := $(DELIVERY_CONFIGDIR)
uninstall: MCPDIR := $(DELIVERY_MCPDIR)
uninstall: UNITDIR := $(DELIVERY_UNITDIR)
uninstall: DOCDIR := $(DELIVERY_DOCDIR)

.PHONY: layout install install-core install-plugins delivery-check-layout \
        delivery-check-install delivery-check-core-install delivery-check-plugins-install \
        delivery-check-inputs delivery-check-core-inputs delivery-check-plugins-inputs test-packaging
delivery-check-layout:
	@case $(call delivery-quote,$(DELIVERY_INSTALL_PROFILE)) in system|user) ;; *) echo 'INSTALL_PROFILE must be system or user' >&2; exit 1;; esac
	@case $(call delivery-quote,$(DELIVERY_PROFILE)) in standalone|legacy-anolisa) ;; *) echo 'Unsupported PROFILE' >&2; exit 1;; esac
	@case $(call delivery-quote,$(DELIVERY_AWPRJ)) in *[!A-Za-z0-9_.-]*|.|..) echo 'AWPRJ must be empty or one namespace segment' >&2; exit 1;; esac
	@if [ $(call delivery-quote,$(DELIVERY_PROFILE)) = legacy-anolisa ] && [ $(call delivery-quote,$(DELIVERY_AWPRJ)) != anolisa ]; then echo 'legacy-anolisa requires AWPRJ=anolisa' >&2; exit 1; fi
	@for path in $(call delivery-quote,$(DELIVERY_PREFIX)) $(call delivery-quote,$(DELIVERY_BINDIR)) $(call delivery-quote,$(DELIVERY_LIBDIR)) $(call delivery-quote,$(DELIVERY_LIBEXECDIR)) $(call delivery-quote,$(DELIVERY_DATADIR)) $(call delivery-quote,$(DELIVERY_ADAPTERDIR)) $(call delivery-quote,$(DELIVERY_UNITDIR)) $(call delivery-quote,$(DELIVERY_DOCDIR)) $(call delivery-quote,$(DELIVERY_CONFIGDIR)) $(call delivery-quote,$(DELIVERY_MCPDIR)); do \
		case "$$path" in /*) ;; *) echo "Layout path must be absolute: $$path" >&2; exit 1;; esac; \
		case "$$path" in *[!A-Za-z0-9_./-]*) echo "Unsupported characters in system layout path: $$path" >&2; exit 1;; esac; \
		case "/$$path/" in */../*|*/./*) echo "Layout path contains traversal: $$path" >&2; exit 1;; esac; \
	done

layout: delivery-check-layout ## Print file installation layout
	@printf '%s\n' $(call delivery-quote,PROFILE=$(DELIVERY_PROFILE)) $(call delivery-quote,INSTALL_PROFILE=$(DELIVERY_INSTALL_PROFILE)) $(call delivery-quote,AWPRJ=$(DELIVERY_AWPRJ)) $(call delivery-quote,PREFIX=$(DELIVERY_PREFIX)) $(call delivery-quote,BINDIR=$(DELIVERY_BINDIR)) \
		$(call delivery-quote,LIBDIR=$(DELIVERY_LIBDIR)) $(call delivery-quote,LIBEXECDIR=$(DELIVERY_LIBEXECDIR)) $(call delivery-quote,DATADIR=$(DELIVERY_DATADIR)) \
		$(call delivery-quote,ADAPTERDIR=$(DELIVERY_ADAPTERDIR)) $(call delivery-quote,UNITDIR=$(DELIVERY_UNITDIR)) $(call delivery-quote,CONFIGDIR=$(DELIVERY_CONFIGDIR)) $(call delivery-quote,MCPDIR=$(DELIVERY_MCPDIR)) \
		'PRODUCTS=core plugins; no bundled skills product' \
		'ACTIVATION=not-performed; existing config/data/unit naming policy retained'

DELIVERY_CORE_WRITE_TARGETS = $(call delivery-quote,file:$(DELIVERY_BINDIR)/agent-memory) \
	$(call delivery-quote,file:$(DELIVERY_UNITDIR)/anolisa-memory@.service) \
	$(call delivery-quote,file:$(DELIVERY_CONFIGDIR)/default.toml) \
	$(call delivery-quote,file:$(DELIVERY_MCPDIR)/agent-memory.json) \
	$(call delivery-quote,file:$(DELIVERY_DOCDIR)/LICENSE)
DELIVERY_PLUGINS_WRITE_TARGETS = $(call delivery-quote,tree:$(DELIVERY_ADAPTERDIR))
DELIVERY_PLUGINS_FILES := manifest.json openclaw/package.json openclaw/openclaw.plugin.json \
	openclaw/dist/index.js openclaw/scripts/detect.sh openclaw/scripts/install.sh \
	openclaw/scripts/uninstall.sh
DELIVERY_PLUGINS_UNINSTALL_TARGETS = $(foreach file,$(DELIVERY_PLUGINS_FILES),$(call delivery-quote,file:$(DELIVERY_ADAPTERDIR)/$(file)))

uninstall: delivery-check-uninstall
delivery-check-uninstall: delivery-check-layout
	python3 $(call delivery-quote,$(DELIVERY_ROOT)/packaging/check-install.py) $(call delivery-quote,$(DESTDIR)) $(DELIVERY_CORE_WRITE_TARGETS) $(DELIVERY_PLUGINS_UNINSTALL_TARGETS)

# Complete installation validates both groups before either group can write.
delivery-check-install: delivery-check-core-install delivery-check-plugins-install

delivery-check-core-install: delivery-check-layout
	python3 $(call delivery-quote,$(DELIVERY_ROOT)/packaging/check-install.py) $(call delivery-quote,$(DESTDIR)) $(DELIVERY_CORE_WRITE_TARGETS)

delivery-check-plugins-install: delivery-check-layout
	python3 $(call delivery-quote,$(DELIVERY_ROOT)/packaging/check-install.py) $(call delivery-quote,$(DESTDIR)) $(DELIVERY_PLUGINS_WRITE_TARGETS)

delivery-check-inputs: delivery-check-core-inputs delivery-check-plugins-inputs

delivery-check-core-inputs:
	@for input in $(call delivery-quote,$(DELIVERY_BINARY)) $(call delivery-quote,$(DELIVERY_ROOT)/LICENSE) $(call delivery-quote,$(DELIVERY_ROOT)/config/default.toml) $(call delivery-quote,$(DELIVERY_ROOT)/config/mcp-server.json) $(call delivery-quote,$(DELIVERY_ROOT)/packaging/render-install.py) $(call delivery-quote,$(DELIVERY_ROOT)/packaging/systemd/anolisa-memory@.service.in); do \
		test -f "$$input" || { \
			hint=; [ "$$input" != $(call delivery-quote,$(DELIVERY_BINARY)) ] || hint='; run make build first'; \
			echo "Missing installation input: $$input$$hint" >&2; exit 1; \
		}; \
	done

delivery-check-plugins-inputs:
	python3 $(call delivery-quote,$(DELIVERY_ROOT)/packaging/stage-adapter.py) --check $(call delivery-quote,$(DELIVERY_ROOT)/adapters/agent-memory)

install-core: delivery-check-core-install delivery-check-core-inputs ## Install prebuilt binary, configuration, MCP descriptor, user unit and license
	$(MAKE) -C $(call delivery-quote,$(DELIVERY_ROOT)) install-core-files install-systemd-user $(DELIVERY_INSTALL_ARGS)
	install -d -m 0755 $(call delivery-quote,$(DESTDIR)$(DELIVERY_DOCDIR))
	install -m 0644 $(call delivery-quote,$(DELIVERY_ROOT)/LICENSE) $(call delivery-quote,$(DESTDIR)$(DELIVERY_DOCDIR)/LICENSE)

install-plugins: delivery-check-plugins-install delivery-check-plugins-inputs ## Install prebuilt Agent plugin files without registration
	$(MAKE) -C $(call delivery-quote,$(DELIVERY_ROOT)) install-adapter-resources-staged $(DELIVERY_INSTALL_ARGS) PLUGIN_PAYLOAD=runtime

install: delivery-check-install delivery-check-inputs ## Install all prebuilt groups without building or activation
	$(MAKE) -C $(call delivery-quote,$(DELIVERY_ROOT)) install-core install-plugins $(DELIVERY_INSTALL_ARGS)
	@echo 'Installed prebuilt files; no build, framework registration, or service activation performed.'
	@echo 'Runtime config/data paths and the opt-in user unit identity are unchanged.'

test-packaging: ## Check file installation and RPM/raw packaging with fixtures
	python3 "$(DELIVERY_ROOT)/tests/test_packaging_layout.py"
	python3 -m unittest discover -s "$(DELIVERY_ROOT)/tests" -p test_raw_packaging.py
