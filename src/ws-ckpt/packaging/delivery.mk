# Included by the component root Makefile; no second public Makefile/profile.
DELIVERY_ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/..)
# File-origin legacy install defaults must not leak into the public file-only installer.
delivery-input = $(if $(filter command line environment override,$(origin $(1))),$($(1)),$(2))
# System file installation; explicit install-legacy retains historical behavior.
DELIVERY_PROFILE := $(call delivery-input,PROFILE,standalone)
DELIVERY_INSTALL_PROFILE := $(call delivery-input,INSTALL_PROFILE,system)
DELIVERY_AWPRJ := $(call delivery-input,AWPRJ,$(if $(filter legacy-anolisa,$(DELIVERY_PROFILE)),anolisa,))
DELIVERY_PREFIX := $(call delivery-input,PREFIX,/usr/local)
DELIVERY_BINDIR := $(call delivery-input,BINDIR,$(if $(filter legacy-anolisa,$(DELIVERY_PROFILE)),/usr/local/bin,$(DELIVERY_PREFIX)/bin))
DELIVERY_LIBDIR := $(call delivery-input,LIBDIR,$(DELIVERY_PREFIX)/lib)
DELIVERY_LIBEXECDIR := $(call delivery-input,LIBEXECDIR,$(DELIVERY_PREFIX)/libexec/$(if $(strip $(DELIVERY_AWPRJ)),$(DELIVERY_AWPRJ)/)ws-ckpt)
# The CLI and daemon share a fixed host configuration path.
DELIVERY_SYSCONFDIR := $(call delivery-input,SYSCONFDIR,/etc)
DELIVERY_UNITDIR := $(call delivery-input,UNITDIR,$(DELIVERY_LIBDIR)/systemd/system)
DELIVERY_CONFIGDIR := $(call delivery-input,CONFIGDIR,$(DELIVERY_SYSCONFDIR)/ws-ckpt)
DELIVERY_DOCDIR := $(call delivery-input,DOCDIR,$(DELIVERY_PREFIX)/share/doc/ws-ckpt)
DELIVERY_BINARY := $(call delivery-input,BINARY,$(DELIVERY_ROOT)/src/target/release/ws-ckpt)

ifeq ($(DELIVERY_PROFILE),legacy-anolisa)
DELIVERY_DATADIR := $(call delivery-input,DATADIR,$(DELIVERY_PREFIX)/share/anolisa)
DELIVERY_ADAPTERDIR := $(call delivery-input,ADAPTERDIR,$(DELIVERY_DATADIR)/adapters/ws-ckpt)
DELIVERY_SKILLDIR := $(call delivery-input,SKILLDIR,$(DELIVERY_DATADIR)/runtime/skills/ws-ckpt)
DELIVERY_PLUGINDIR := $(call delivery-input,PLUGINDIR,$(DELIVERY_DATADIR)/runtime/ws-ckpt/plugins)
DELIVERY_COMPONENTDIR := $(call delivery-input,COMPONENTDIR,$(DELIVERY_DATADIR)/components/ws-ckpt)
DELIVERY_SECOND_SKILLDIR := $(call delivery-input,SECOND_SKILLDIR,$(DELIVERY_DATADIR)/skills/ws-ckpt)
else
DELIVERY_DATADIR := $(call delivery-input,DATADIR,$(DELIVERY_PREFIX)/share/$(if $(strip $(DELIVERY_AWPRJ)),$(DELIVERY_AWPRJ)/)ws-ckpt)
DELIVERY_ADAPTERDIR := $(call delivery-input,ADAPTERDIR,$(DELIVERY_DATADIR)/adapters)
DELIVERY_SKILLDIR := $(call delivery-input,SKILLDIR,$(DELIVERY_DATADIR)/skills)
DELIVERY_PLUGINDIR := $(call delivery-input,PLUGINDIR,$(DELIVERY_LIBEXECDIR)/plugins)
DELIVERY_COMPONENTDIR := $(call delivery-input,COMPONENTDIR,$(DELIVERY_DATADIR)/component)
DELIVERY_SECOND_SKILLDIR := $(call delivery-input,SECOND_SKILLDIR,$(DELIVERY_SKILLDIR))
endif

DELIVERY_INSTALL_ARGS = INSTALL_PROFILE=system $(call delivery-quote,DESTDIR=$(DESTDIR)) $(call delivery-quote,BINARY=$(DELIVERY_BINARY)) \
	$(call delivery-quote,BINDIR=$(DELIVERY_BINDIR)) $(call delivery-quote,LIBDIR=$(DELIVERY_LIBDIR)) $(call delivery-quote,DATADIR=$(DELIVERY_DATADIR)) \
	$(call delivery-quote,ADAPTER_DIR=$(DELIVERY_ADAPTERDIR)) $(call delivery-quote,PLUGINS_DIR=$(DELIVERY_PLUGINDIR)) \
	$(call delivery-quote,RUNTIME_SKILLS_DIR=$(DELIVERY_SKILLDIR)) $(call delivery-quote,COSH_SKILLS_DIR=$(DELIVERY_SECOND_SKILLDIR)) \
	$(call delivery-quote,COMPONENT_DIR=$(DELIVERY_COMPONENTDIR)) $(call delivery-quote,SYSTEMD_SYSTEM_DIR=$(DELIVERY_UNITDIR)) $(call delivery-quote,CONFIG_DIR=$(DELIVERY_CONFIGDIR))

delivery-quote = '$(subst ','"'"',$(1))'

.PHONY: layout install delivery-check-layout delivery-check-install delivery-check-inputs test-packaging
delivery-check-layout:
	@test $(call delivery-quote,$(DELIVERY_INSTALL_PROFILE)) = system || { echo 'File-only install supports INSTALL_PROFILE=system only; use install-legacy for historical profiles' >&2; exit 1; }
	@case $(call delivery-quote,$(DELIVERY_PROFILE)) in standalone|legacy-anolisa) ;; *) echo 'Unsupported PROFILE' >&2; exit 1;; esac
	@case $(call delivery-quote,$(DELIVERY_AWPRJ)) in *[!A-Za-z0-9_.-]*|.|..) echo 'AWPRJ must be empty or one namespace segment' >&2; exit 1;; esac
	@if [ $(call delivery-quote,$(DELIVERY_PROFILE)) = legacy-anolisa ] && [ $(call delivery-quote,$(DELIVERY_AWPRJ)) != anolisa ]; then echo 'legacy-anolisa requires AWPRJ=anolisa' >&2; exit 1; fi
	@for path in $(call delivery-quote,$(DELIVERY_PREFIX)) $(call delivery-quote,$(DELIVERY_BINDIR)) $(call delivery-quote,$(DELIVERY_LIBDIR)) $(call delivery-quote,$(DELIVERY_LIBEXECDIR)) $(call delivery-quote,$(DELIVERY_DATADIR)) $(call delivery-quote,$(DELIVERY_ADAPTERDIR)) $(call delivery-quote,$(DELIVERY_SKILLDIR)) $(call delivery-quote,$(DELIVERY_PLUGINDIR)) $(call delivery-quote,$(DELIVERY_UNITDIR)) $(call delivery-quote,$(DELIVERY_DOCDIR)) $(call delivery-quote,$(DELIVERY_CONFIGDIR)) $(call delivery-quote,$(DELIVERY_COMPONENTDIR)) $(call delivery-quote,$(DELIVERY_SECOND_SKILLDIR)); do \
		case "$$path" in /*) ;; *) echo "Layout path must be absolute: $$path" >&2; exit 1;; esac; \
		case "$$path" in *[!A-Za-z0-9_./-]*) echo "Unsupported characters in system layout path: $$path" >&2; exit 1;; esac; \
		case "/$$path/" in */../*|*/./*) echo "Layout path contains traversal: $$path" >&2; exit 1;; esac; \
	done

layout: delivery-check-layout ## Print file installation layout
	@printf '%s\n' $(call delivery-quote,PROFILE=$(DELIVERY_PROFILE)) $(call delivery-quote,INSTALL_PROFILE=$(DELIVERY_INSTALL_PROFILE)) $(call delivery-quote,AWPRJ=$(DELIVERY_AWPRJ)) $(call delivery-quote,PREFIX=$(DELIVERY_PREFIX)) $(call delivery-quote,BINDIR=$(DELIVERY_BINDIR)) \
		$(call delivery-quote,LIBDIR=$(DELIVERY_LIBDIR)) $(call delivery-quote,LIBEXECDIR=$(DELIVERY_LIBEXECDIR)) $(call delivery-quote,DATADIR=$(DELIVERY_DATADIR)) \
		$(call delivery-quote,ADAPTERDIR=$(DELIVERY_ADAPTERDIR)) $(call delivery-quote,PLUGINDIR=$(DELIVERY_PLUGINDIR)) $(call delivery-quote,SKILLDIR=$(DELIVERY_SKILLDIR)) \
		$(call delivery-quote,UNITDIR=$(DELIVERY_UNITDIR)) $(call delivery-quote,DOCDIR=$(DELIVERY_DOCDIR)) $(call delivery-quote,CONFIGDIR=$(DELIVERY_CONFIGDIR)) \
		'PRODUCTS=core plugins skills' 'ACTIVATION=not-performed; plugin discovery requires owner decision'

DELIVERY_CORE_WRITE_TARGETS = $(call delivery-quote,file:$(DELIVERY_BINDIR)/ws-ckpt) \
	$(call delivery-quote,file:$(DELIVERY_UNITDIR)/ws-ckpt.service) \
	$(call delivery-quote,file:$(DELIVERY_CONFIGDIR)/config.toml) \
	$(call delivery-quote,file:$(DELIVERY_CONFIGDIR)/config.toml.sample) \
	$(call delivery-quote,file:$(DELIVERY_DOCDIR)/LICENSE)
DELIVERY_PLUGINS_WRITE_TARGETS = $(call delivery-quote,tree:$(DELIVERY_ADAPTERDIR)) \
	$(call delivery-quote,tree:$(DELIVERY_PLUGINDIR))
DELIVERY_SKILLS_WRITE_TARGETS = $(call delivery-quote,tree:$(DELIVERY_SKILLDIR))
ifneq ($(DELIVERY_SKILLDIR),$(DELIVERY_SECOND_SKILLDIR))
DELIVERY_SKILLS_WRITE_TARGETS += $(call delivery-quote,tree:$(DELIVERY_SECOND_SKILLDIR))
endif
ifeq ($(DELIVERY_PROFILE),legacy-anolisa)
DELIVERY_CORE_WRITE_TARGETS += $(call delivery-quote,file:$(DELIVERY_COMPONENTDIR)/component.toml)
endif

.PHONY: install-core install-plugins install-skills delivery-check-core-install \
        delivery-check-plugins-install delivery-check-skills-install delivery-check-core-inputs \
        delivery-check-plugins-inputs delivery-check-skills-inputs

# The aggregate preflight completes before any selected group writes a file.
delivery-check-install: delivery-check-core-install delivery-check-plugins-install delivery-check-skills-install

delivery-check-core-install: delivery-check-layout
	python3 $(call delivery-quote,$(DELIVERY_ROOT)/packaging/check-install.py) $(call delivery-quote,$(DESTDIR)) $(DELIVERY_CORE_WRITE_TARGETS)

delivery-check-plugins-install: delivery-check-layout
	python3 $(call delivery-quote,$(DELIVERY_ROOT)/packaging/check-install.py) $(call delivery-quote,$(DESTDIR)) $(DELIVERY_PLUGINS_WRITE_TARGETS)

delivery-check-skills-install: delivery-check-layout
	python3 $(call delivery-quote,$(DELIVERY_ROOT)/packaging/check-install.py) $(call delivery-quote,$(DESTDIR)) $(DELIVERY_SKILLS_WRITE_TARGETS)

delivery-check-inputs: delivery-check-core-inputs delivery-check-plugins-inputs delivery-check-skills-inputs

delivery-check-core-inputs:
	@for input in $(call delivery-quote,$(DELIVERY_BINARY)) $(call delivery-quote,$(DELIVERY_ROOT)/LICENSE) $(call delivery-quote,$(DELIVERY_ROOT)/src/config.toml.sample) $(call delivery-quote,$(DELIVERY_ROOT)/packaging/systemd/ws-ckpt.service.in); do \
		test -f "$$input" || { \
			hint=; [ "$$input" != $(call delivery-quote,$(DELIVERY_BINARY)) ] || hint='; run make build first'; \
			echo "Missing installation input: $$input$$hint" >&2; exit 1; \
		}; \
	done
ifeq ($(DELIVERY_PROFILE),legacy-anolisa)
	@test -f $(call delivery-quote,$(DELIVERY_ROOT)/packaging/anolisa/rpm/component.toml) || { echo "Missing ANOLISA RPM contract" >&2; exit 1; }
endif

delivery-check-plugins-inputs:
	@for input in $(call delivery-quote,$(DELIVERY_ROOT)/src/plugins/openclaw/dist/src/index.js) $(call delivery-quote,$(DELIVERY_ROOT)/src/plugins/openclaw/package.json) $(call delivery-quote,$(DELIVERY_ROOT)/src/plugins/openclaw/openclaw.plugin.json) $(call delivery-quote,$(DELIVERY_ROOT)/src/plugins/hermes/plugin.yaml) \
		$(foreach script,lib-discover.sh lib-openclaw.sh detect-openclaw.sh install-openclaw.sh uninstall-openclaw.sh,$(call delivery-quote,$(DELIVERY_ROOT)/scripts/openclaw/$(script))) \
		$(foreach script,lib-discover.sh detect-hermes.sh install-hermes.sh uninstall-hermes.sh,$(call delivery-quote,$(DELIVERY_ROOT)/scripts/hermes/$(script))); do \
		test -f "$$input" || { \
			hint=; [ "$$input" != $(call delivery-quote,$(DELIVERY_ROOT)/src/plugins/openclaw/dist/src/index.js) ] || hint='; run make build first'; \
			echo "Missing installation input: $$input$$hint" >&2; exit 1; \
		}; \
	done

delivery-check-skills-inputs:
	@test -f $(call delivery-quote,$(DELIVERY_ROOT)/src/skills/ws-ckpt/SKILL.md) || { echo 'Missing installation input: ws-ckpt/SKILL.md' >&2; exit 1; }

install-core: delivery-check-core-install delivery-check-core-inputs ## Install the prebuilt CLI, configuration, service and license
	$(MAKE) -C $(call delivery-quote,$(DELIVERY_ROOT)) install-core-files $(DELIVERY_INSTALL_ARGS)
	install -d -m 0755 $(call delivery-quote,$(DESTDIR)$(DELIVERY_DOCDIR))
	install -m 0644 $(call delivery-quote,$(DELIVERY_ROOT)/LICENSE) $(call delivery-quote,$(DESTDIR)$(DELIVERY_DOCDIR)/LICENSE)
ifeq ($(DELIVERY_PROFILE),legacy-anolisa)
	$(MAKE) -C $(call delivery-quote,$(DELIVERY_ROOT)) install-component-metadata $(DELIVERY_INSTALL_ARGS)
endif

install-plugins: delivery-check-plugins-install delivery-check-plugins-inputs ## Install prebuilt Agent plugins without registration
	$(MAKE) -C $(call delivery-quote,$(DELIVERY_ROOT)) install-plugin-files $(DELIVERY_INSTALL_ARGS)

install-skills: delivery-check-skills-install delivery-check-skills-inputs ## Install the bundled checkpoint skill without a binary build
	$(MAKE) -C $(call delivery-quote,$(DELIVERY_ROOT)) install-skills-files $(DELIVERY_INSTALL_ARGS)

install: delivery-check-install delivery-check-inputs ## Install all prebuilt groups without building or activation
	$(MAKE) -C $(call delivery-quote,$(DELIVERY_ROOT)) install-core install-plugins install-skills $(DELIVERY_INSTALL_ARGS)
	@echo 'Installed prebuilt files; no build, framework registration, or service activation performed.'
	@echo 'Runtime discovery and configuration-path policies are unchanged.'

test-packaging: ## Check file installation and RPM/raw packaging with fixtures
	python3 "$(DELIVERY_ROOT)/tests/test_packaging_layout.py"
	python3 -m unittest discover -s "$(DELIVERY_ROOT)/tests" -p test_raw_packaging.py -k legacy_stage
