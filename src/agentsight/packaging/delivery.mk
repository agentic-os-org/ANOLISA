# Included by the component root Makefile; no separate public build entrypoint.
# Legacy file defaults must not leak into the public file installation.
delivery_input = $(if $(filter command line environment,$(origin $(1))),$($(1)),$(2))
DELIVERY_ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/..)
DELIVERY_AWPRJ := $(call delivery_input,AWPRJ,)
DELIVERY_INSTALL_PROFILE := $(call delivery_input,INSTALL_PROFILE,system)
DELIVERY_PREFIX := $(call delivery_input,PREFIX,/usr/local)
delivery_trim_slashes = $(if $(filter %/,$(1)),$(call delivery_trim_slashes,$(patsubst %/,%,$(1))),$(1))
DELIVERY_PREFIX_BASE := $(call delivery_trim_slashes,$(DELIVERY_PREFIX))
DELIVERY_DESTDIR := $(call delivery_input,DESTDIR,)
DELIVERY_NAMESPACE := $(if $(strip $(DELIVERY_AWPRJ)),$(DELIVERY_AWPRJ)/,)
DELIVERY_BINDIR := $(call delivery_input,BINDIR,$(DELIVERY_PREFIX_BASE)/bin)
DELIVERY_LIBDIR := $(call delivery_input,LIBDIR,$(DELIVERY_PREFIX_BASE)/lib/$(DELIVERY_NAMESPACE)agentsight)
DELIVERY_LIBEXECDIR := $(call delivery_input,LIBEXECDIR,$(DELIVERY_PREFIX_BASE)/libexec/$(DELIVERY_NAMESPACE)agentsight)
DELIVERY_DATADIR := $(call delivery_input,DATADIR,$(DELIVERY_PREFIX_BASE)/share/$(DELIVERY_NAMESPACE)agentsight)
DELIVERY_ADAPTERDIR := $(call delivery_input,ADAPTERDIR,$(DELIVERY_DATADIR)/adapters)
DELIVERY_SKILLDIR := $(call delivery_input,SKILLDIR,$(DELIVERY_DATADIR)/skills)
DELIVERY_SYSCONFDIR := $(call delivery_input,SYSCONFDIR,$(if $(filter /usr,$(DELIVERY_PREFIX_BASE)),/etc,$(DELIVERY_PREFIX_BASE)/etc))
DELIVERY_CONFIGDIR := $(call delivery_input,CONFIGDIR,$(DELIVERY_SYSCONFDIR)/agentsight)
DELIVERY_UNITDIR := $(call delivery_input,UNITDIR,$(DELIVERY_PREFIX_BASE)/lib/systemd/system)
DELIVERY_AGENTSIGHT_BIN := $(call delivery_input,AGENTSIGHT_BIN,$(DELIVERY_ROOT)/target/release/agentsight)
DELIVERY_CORE_BINARY := $(abspath $(DELIVERY_AGENTSIGHT_BIN))
DELIVERY_ENFORCER_BIN := $(call delivery_input,ENFORCER_BIN,$(DELIVERY_ROOT)/target/delivery/agentsight-enforcer)
DELIVERY_ENFORCER_SOURCE := $(if $(strip $(DELIVERY_ENFORCER_BIN)),$(abspath $(DELIVERY_ENFORCER_BIN)),)

# User-profile removal belongs to the retained legacy installer. System removal
# follows public installation while honoring explicit legacy path arguments.
ifeq ($(DELIVERY_INSTALL_PROFILE),user)
DELIVERY_UNINSTALL_BINDIR := $(BINDIR)
DELIVERY_UNINSTALL_UNITDIR := $(SYSTEMD_SYSTEM_DIR)
DELIVERY_UNINSTALL_CONFIGDIR := $(AGENTSIGHT_CONFIG_DIR)
else
DELIVERY_UNINSTALL_BINDIR := $(DELIVERY_BINDIR)
DELIVERY_UNINSTALL_UNITDIR := $(call delivery_input,UNITDIR,$(call delivery_input,SYSTEMD_SYSTEM_DIR,$(DELIVERY_UNITDIR)))
DELIVERY_UNINSTALL_CONFIGDIR := $(call delivery_input,CONFIGDIR,$(call delivery_input,AGENTSIGHT_CONFIG_DIR,$(DELIVERY_CONFIGDIR)))
endif
DELIVERY_UNINSTALL_FILES = $(addprefix $(DELIVERY_UNINSTALL_BINDIR)/,agentsight agentsight-enforcer agentsight-start) \
	$(DELIVERY_UNINSTALL_UNITDIR)/agentsight.service $(DELIVERY_UNINSTALL_UNITDIR)/agentsight-enforcer.service \
	$(DELIVERY_UNINSTALL_CONFIGDIR)/config.json
DELIVERY_UNINSTALL_DIRS :=
ifneq ($(DELIVERY_INSTALL_PROFILE),user)
# Remove only the declared DSH entrypoints; keep user additions under lib.
DELIVERY_UNINSTALL_FILES += $(DELIVERY_DATADIR)/LICENSE \
	$(DELIVERY_ADAPTERDIR)/dsh/package.json $(DELIVERY_ADAPTERDIR)/dsh/cordis.patch.yml \
	$(DELIVERY_ADAPTERDIR)/dsh/lib/index.js $(DELIVERY_ADAPTERDIR)/dsh/lib/types/index.d.ts
DELIVERY_UNINSTALL_DIRS := $(DELIVERY_ADAPTERDIR)/dsh/lib/types \
	$(DELIVERY_ADAPTERDIR)/dsh/lib $(DELIVERY_ADAPTERDIR)/dsh
endif

.PHONY: delivery-validate-layout delivery-validate-install layout install test-packaging

delivery-validate-layout:
	@test "$(DELIVERY_INSTALL_PROFILE)" = system || { echo 'Public install supports INSTALL_PROFILE=system only; use an explicit legacy installer for other profiles' >&2; exit 1; }
	@case "$(DELIVERY_AWPRJ)" in *[!a-zA-Z0-9._-]*|.|..) echo 'AWPRJ must be empty or a single namespace name' >&2; exit 1;; esac
	@for path in "$(DELIVERY_PREFIX)" "$(DELIVERY_BINDIR)" "$(DELIVERY_LIBDIR)" "$(DELIVERY_LIBEXECDIR)" "$(DELIVERY_DATADIR)" "$(DELIVERY_ADAPTERDIR)" "$(DELIVERY_SKILLDIR)" "$(DELIVERY_UNITDIR)" "$(DELIVERY_SYSCONFDIR)" "$(DELIVERY_CONFIGDIR)"; do \
		case "$$path" in /*) ;; *) echo "Expected an absolute system path: $$path" >&2; exit 1;; esac; \
		case "$$path" in *[!a-zA-Z0-9_./-]*|*/../*|*/./*|*/..|*/.) echo "Expected a normalized system path using letters, digits, _, ., / or -: $$path" >&2; exit 1;; esac; \
	done

layout: delivery-validate-layout ## Show the public build/install layout
	@printf '%s\n' 'INSTALL_PROFILE=system' 'AWPRJ=$(DELIVERY_AWPRJ)' 'PREFIX=$(DELIVERY_PREFIX)' 'DESTDIR=$(DELIVERY_DESTDIR)' 'BINDIR=$(DELIVERY_BINDIR)' \
		'LIBDIR=$(DELIVERY_LIBDIR)' 'LIBEXECDIR=$(DELIVERY_LIBEXECDIR)' 'DATADIR=$(DELIVERY_DATADIR)' \
		'ADAPTERDIR=$(DELIVERY_ADAPTERDIR)' 'SKILLDIR=$(DELIVERY_SKILLDIR)' 'UNITDIR=$(DELIVERY_UNITDIR)' \
		'SYSCONFDIR=$(DELIVERY_SYSCONFDIR)' 'CONFIGDIR=$(DELIVERY_CONFIGDIR)'

# Each role checks only its own inputs and destination paths.
DELIVERY_CORE_INPUTS = \
	--executable "$(DELIVERY_CORE_BINARY)" \
	--executable "$(DELIVERY_ENFORCER_SOURCE)" \
	--file "$(DELIVERY_ENFORCER_SOURCE).sha256" \
	--file "$(DELIVERY_ROOT)/agentsight.json" \
	--file "$(DELIVERY_ROOT)/LICENSE" \
	--file "$(DELIVERY_ROOT)/packaging/systemd/render.py" \
	--file "$(DELIVERY_ROOT)/packaging/systemd/agentsight-start.sh.in" \
	--file "$(DELIVERY_ROOT)/packaging/systemd/agentsight.service.in" \
	--file "$(DELIVERY_ROOT)/packaging/systemd/agentsight-enforcer.service.in" \
	--target-file "$(DELIVERY_BINDIR)/agentsight" \
	--target-file "$(DELIVERY_BINDIR)/agentsight-enforcer" \
	--target-file "$(DELIVERY_BINDIR)/agentsight-start" \
	--target-file "$(DELIVERY_UNITDIR)/agentsight.service" \
	--target-file "$(DELIVERY_UNITDIR)/agentsight-enforcer.service" \
	--target-file "$(DELIVERY_CONFIGDIR)/config.json" \
	--target-file "$(DELIVERY_DATADIR)/LICENSE"
DELIVERY_PLUGIN_INPUTS = \
	--directory "$(DELIVERY_ROOT)/dsh-plugin/lib" \
	--file "$(DELIVERY_ROOT)/dsh-plugin/lib/index.js" \
	--file "$(DELIVERY_ROOT)/dsh-plugin/package.json" \
	--file "$(DELIVERY_ROOT)/dsh-plugin/cordis.patch.yml" \
	--tree "$(DELIVERY_ROOT)/dsh-plugin/lib" "$(DELIVERY_ADAPTERDIR)/dsh/lib" \
	--target-file "$(DELIVERY_ADAPTERDIR)/dsh/package.json" \
	--target-file "$(DELIVERY_ADAPTERDIR)/dsh/cordis.patch.yml"

.PHONY: delivery-validate-core delivery-validate-plugins delivery-install-core-files \
	delivery-install-plugins-files install-core install-plugins

delivery-validate-core: delivery-validate-layout
	@python3 "$(DELIVERY_ROOT)/packaging/validate-stage.py" --destdir "$(DELIVERY_DESTDIR)" $(DELIVERY_CORE_INPUTS)
	@python3 "$(DELIVERY_ROOT)/scripts/copy-enforcer.py" --verify "$(DELIVERY_ENFORCER_SOURCE)"

delivery-validate-plugins: delivery-validate-layout
	@python3 "$(DELIVERY_ROOT)/packaging/validate-stage.py" --destdir "$(DELIVERY_DESTDIR)" $(DELIVERY_PLUGIN_INPUTS)

delivery-validate-install: delivery-validate-core delivery-validate-plugins

delivery-install-core-files:
	$(MAKE) -C "$(DELIVERY_ROOT)" install-prebuilt \
		INSTALL_PROFILE=system ENFORCER_BIN="$(DELIVERY_ENFORCER_SOURCE)" AGENTSIGHT_BIN="$(DELIVERY_CORE_BINARY)" \
		DESTDIR="$(DELIVERY_DESTDIR)" PREFIX="$(DELIVERY_PREFIX)" BINDIR="$(DELIVERY_BINDIR)" \
		SYSCONFDIR="$(DELIVERY_SYSCONFDIR)" AGENTSIGHT_CONFIG_DIR="$(DELIVERY_CONFIGDIR)" SYSTEMD_SYSTEM_DIR="$(DELIVERY_UNITDIR)" \
		SERVICE_BINDIR="$(DELIVERY_BINDIR)" START_BINARY="$(DELIVERY_BINDIR)/agentsight" START_CONFIG="$(DELIVERY_CONFIGDIR)/config.json" \
		SETCAP=0 INSTALL_SYSTEMD=1
	install -d -m 0755 "$(DELIVERY_DESTDIR)$(DELIVERY_DATADIR)"
	install -p -m 0644 "$(DELIVERY_ROOT)/LICENSE" "$(DELIVERY_DESTDIR)$(DELIVERY_DATADIR)/LICENSE"

delivery-install-plugins-files:
	install -d -m 0755 "$(DELIVERY_DESTDIR)$(DELIVERY_ADAPTERDIR)/dsh/lib"
	cp -Rp "$(DELIVERY_ROOT)/dsh-plugin/lib/." "$(DELIVERY_DESTDIR)$(DELIVERY_ADAPTERDIR)/dsh/lib/"
	install -p -m 0644 "$(DELIVERY_ROOT)/dsh-plugin/package.json" \
		"$(DELIVERY_ROOT)/dsh-plugin/cordis.patch.yml" "$(DELIVERY_DESTDIR)$(DELIVERY_ADAPTERDIR)/dsh/"

install-core: delivery-validate-core ## Install prebuilt programs, services, configuration and license
	$(MAKE) -C "$(DELIVERY_ROOT)" delivery-install-core-files

install-plugins: delivery-validate-plugins ## Install built DSH resources without framework registration
	$(MAKE) -C "$(DELIVERY_ROOT)" delivery-install-plugins-files

# Preflight every applicable role before any copying starts.
install: delivery-validate-install ## Install all prebuilt files; DESTDIR optionally adds a temporary root
	$(MAKE) -C "$(DELIVERY_ROOT)" delivery-install-core-files delivery-install-plugins-files

.PHONY: delivery-validate-uninstall
delivery-validate-uninstall:
	@case "$(DELIVERY_INSTALL_PROFILE)" in system|user) ;; *) echo 'INSTALL_PROFILE must be system or user' >&2; exit 1;; esac
	@for path in "$(DELIVERY_UNINSTALL_BINDIR)" "$(DELIVERY_UNINSTALL_UNITDIR)" "$(DELIVERY_UNINSTALL_CONFIGDIR)"; do \
		case "$$path" in /*) ;; *) echo "Expected an absolute uninstall path: $$path" >&2; exit 1;; esac; \
		case "$$path" in *[!a-zA-Z0-9_./-]*|*/../*|*/./*|*/..|*/.) echo "Expected a normalized uninstall path: $$path" >&2; exit 1;; esac; \
	done
	@python3 "$(DELIVERY_ROOT)/packaging/validate-stage.py" --destdir "$(DESTDIR)" \
		$(foreach path,$(DELIVERY_UNINSTALL_FILES),--target-file "$(path)") \
		$(foreach path,$(DELIVERY_UNINSTALL_DIRS),--target-directory "$(path)")

ifneq ($(DELIVERY_INSTALL_PROFILE),user)
delivery-validate-uninstall: delivery-validate-layout
endif

uninstall: delivery-validate-uninstall

test-packaging: ## Verify public installation and RPM ownership with fixtures only
	python3 "$(DELIVERY_ROOT)/scripts/benchmark/tests/test_delivery.py"
