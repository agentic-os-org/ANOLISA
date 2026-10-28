# Included by the component root Makefile; no second public Makefile/profile.
.PHONY: test-rpm-packages
test-packaging: test-rpm-packages
test-rpm-packages:
	python3 tests/test_rpm_packages.py

DELIVERY_ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/..)
# File-origin legacy install defaults must not leak into the public file-only installer.
delivery-input = $(if $(filter command line environment override,$(origin $(1))),$($(1)),$(2))
# System file installation; raw and RPM retain their legacy contracts.
DELIVERY_PROFILE := $(call delivery-input,PROFILE,standalone)
DELIVERY_INSTALL_PROFILE := $(call delivery-input,INSTALL_PROFILE,system)
DELIVERY_AWPRJ := $(call delivery-input,AWPRJ,$(if $(filter legacy-anolisa,$(DELIVERY_PROFILE)),anolisa,))
DELIVERY_PREFIX := $(call delivery-input,PREFIX,/usr/local)
DELIVERY_BINDIR := $(call delivery-input,BINDIR,$(if $(filter legacy-anolisa,$(DELIVERY_PROFILE)),/usr/local/bin,$(DELIVERY_PREFIX)/bin))
DELIVERY_DOCDIR := $(call delivery-input,DOCDIR,$(DELIVERY_PREFIX)/share/doc/skillfs)
DELIVERY_BINARY := $(call delivery-input,BINARY,$(DELIVERY_ROOT)/target/release/skillfs)

ifeq ($(DELIVERY_PROFILE),legacy-anolisa)
DELIVERY_DATADIR := $(call delivery-input,DATADIR,$(DELIVERY_PREFIX)/share/anolisa)
DELIVERY_SKILLDIR := $(call delivery-input,SKILLDIR,$(DELIVERY_DATADIR)/skills/skillfs-mount)
DELIVERY_COMPONENTDIR := $(call delivery-input,COMPONENTDIR,$(DELIVERY_DATADIR)/components/skillfs)
else
DELIVERY_DATADIR := $(call delivery-input,DATADIR,$(DELIVERY_PREFIX)/share/$(if $(strip $(DELIVERY_AWPRJ)),$(DELIVERY_AWPRJ)/)skillfs)
DELIVERY_SKILLDIR := $(call delivery-input,SKILLDIR,$(if $(filter command line environment override,$(origin DATADIR)),$(DELIVERY_DATADIR)/skills/skillfs-mount,$(if $(strip $(DELIVERY_AWPRJ)),$(DELIVERY_PREFIX)/share/$(DELIVERY_AWPRJ)/skills/skillfs-mount,$(DELIVERY_DATADIR)/skills/skillfs-mount)))
DELIVERY_COMPONENTDIR := $(call delivery-input,COMPONENTDIR,$(DELIVERY_DATADIR)/component)
endif


delivery-quote = '$(subst ','"'"',$(1))'

.PHONY: layout install delivery-check-layout delivery-check-install delivery-check-inputs test-packaging
delivery-check-layout:
	@test $(call delivery-quote,$(DELIVERY_INSTALL_PROFILE)) = system || { echo 'File-only install supports INSTALL_PROFILE=system only' >&2; exit 1; }
	@case $(call delivery-quote,$(DELIVERY_PROFILE)) in standalone|legacy-anolisa) ;; *) echo 'Unsupported PROFILE' >&2; exit 1;; esac
	@case $(call delivery-quote,$(DELIVERY_AWPRJ)) in *[!A-Za-z0-9_.-]*|.|..) echo 'AWPRJ must be empty or one namespace segment' >&2; exit 1;; esac
	@if [ $(call delivery-quote,$(DELIVERY_PROFILE)) = legacy-anolisa ] && [ $(call delivery-quote,$(DELIVERY_AWPRJ)) != anolisa ]; then echo 'legacy-anolisa requires AWPRJ=anolisa' >&2; exit 1; fi
	@for path in $(call delivery-quote,$(DELIVERY_PREFIX)) $(call delivery-quote,$(DELIVERY_BINDIR)) $(call delivery-quote,$(DELIVERY_DATADIR)) $(call delivery-quote,$(DELIVERY_SKILLDIR)) $(call delivery-quote,$(DELIVERY_DOCDIR)) $(call delivery-quote,$(DELIVERY_COMPONENTDIR)); do \
		case "$$path" in /*) ;; *) echo "Layout path must be absolute: $$path" >&2; exit 1;; esac; \
		case "$$path" in *[!A-Za-z0-9_./-]*) echo "Unsupported characters in system layout path: $$path" >&2; exit 1;; esac; \
		case "/$$path/" in */../*|*/./*) echo "Layout path contains traversal: $$path" >&2; exit 1;; esac; \
	done

layout: delivery-check-layout ## Print file installation layout
	@printf '%s\n' $(call delivery-quote,PROFILE=$(DELIVERY_PROFILE)) $(call delivery-quote,INSTALL_PROFILE=$(DELIVERY_INSTALL_PROFILE)) $(call delivery-quote,AWPRJ=$(DELIVERY_AWPRJ)) $(call delivery-quote,PREFIX=$(DELIVERY_PREFIX)) $(call delivery-quote,BINDIR=$(DELIVERY_BINDIR)) \
		$(call delivery-quote,DATADIR=$(DELIVERY_DATADIR)) $(call delivery-quote,SKILLDIR=$(DELIVERY_SKILLDIR)) $(call delivery-quote,DOCDIR=$(DELIVERY_DOCDIR)) \
		'PRODUCTS=core skills; no plugin product' 'ACTIVATION=not-performed'

DELIVERY_CORE_WRITE_TARGETS = $(call delivery-quote,file:$(DELIVERY_BINDIR)/skillfs) \
	$(call delivery-quote,file:$(DELIVERY_DOCDIR)/LICENSE) \
	$(call delivery-quote,file:$(DELIVERY_DOCDIR)/README.md)
DELIVERY_SKILLS_WRITE_TARGETS = $(call delivery-quote,tree:$(DELIVERY_SKILLDIR))
ifeq ($(DELIVERY_PROFILE),legacy-anolisa)
DELIVERY_CORE_WRITE_TARGETS += $(call delivery-quote,file:$(DELIVERY_COMPONENTDIR)/component.toml)
endif

.PHONY: install-core install-skills delivery-check-core-install delivery-check-skills-install \
        delivery-check-core-inputs delivery-check-skills-inputs

# The aggregate preflight completes before either group writes a file.
delivery-check-install: delivery-check-core-install delivery-check-skills-install

delivery-check-core-install: delivery-check-layout
	python3 $(call delivery-quote,$(DELIVERY_ROOT)/packaging/check-install.py) $(call delivery-quote,$(DESTDIR)) $(DELIVERY_CORE_WRITE_TARGETS)

delivery-check-skills-install: delivery-check-layout
	python3 $(call delivery-quote,$(DELIVERY_ROOT)/packaging/check-install.py) $(call delivery-quote,$(DESTDIR)) $(DELIVERY_SKILLS_WRITE_TARGETS)

delivery-check-inputs: delivery-check-core-inputs delivery-check-skills-inputs

delivery-check-core-inputs:
	@for input in $(call delivery-quote,$(DELIVERY_BINARY)) $(call delivery-quote,$(DELIVERY_ROOT)/LICENSE) $(call delivery-quote,$(DELIVERY_ROOT)/README.md); do \
		test -f "$$input" || { \
			hint=; [ "$$input" != $(call delivery-quote,$(DELIVERY_BINARY)) ] || hint='; run make build first'; \
			echo "Missing installation input: $$input$$hint" >&2; exit 1; \
		}; \
	done
ifeq ($(DELIVERY_PROFILE),legacy-anolisa)
	@test -f $(call delivery-quote,$(DELIVERY_ROOT)/packaging/anolisa/rpm/component.toml) || { echo "Missing ANOLISA RPM contract" >&2; exit 1; }
endif

delivery-check-skills-inputs:
	@test -f $(call delivery-quote,$(DELIVERY_ROOT)/docs/skills/skillfs-mount/SKILL.md) || { echo 'Missing installation input: skillfs-mount/SKILL.md' >&2; exit 1; }

DELIVERY_INSTALL_ARGS = SOURCE_DIR=$(call delivery-quote,$(DELIVERY_ROOT)) \
	$(call delivery-quote,DESTDIR=$(DESTDIR)) $(call delivery-quote,BINARY=$(DELIVERY_BINARY)) $(call delivery-quote,BINDIR=$(DELIVERY_BINDIR)) \
	$(call delivery-quote,DOCDIR=$(DELIVERY_DOCDIR)) $(call delivery-quote,SKILLDIR=$(DELIVERY_SKILLDIR))

install-core: delivery-check-core-install delivery-check-core-inputs ## Install the prebuilt CLI, license and documentation
	$(MAKE) -C $(call delivery-quote,$(DELIVERY_ROOT)) install-core-files $(DELIVERY_INSTALL_ARGS)
ifeq ($(DELIVERY_PROFILE),legacy-anolisa)
	install -d -m 0755 $(call delivery-quote,$(DESTDIR)$(DELIVERY_COMPONENTDIR))
	install -m 0644 $(call delivery-quote,$(DELIVERY_ROOT)/packaging/anolisa/rpm/component.toml) $(call delivery-quote,$(DESTDIR)$(DELIVERY_COMPONENTDIR)/component.toml)
endif

install-skills: delivery-check-skills-install delivery-check-skills-inputs ## Install the bundled mount skill without a binary build
	$(MAKE) -C $(call delivery-quote,$(DELIVERY_ROOT)) install-skills-files $(DELIVERY_INSTALL_ARGS)

install: delivery-check-install delivery-check-inputs ## Install all prebuilt groups without activation
	$(MAKE) -C $(call delivery-quote,$(DELIVERY_ROOT)) install-core install-skills $(DELIVERY_INSTALL_ARGS)
	@echo 'Installed prebuilt files; no build, framework registration, or service activation performed.'

test-packaging: ## Check file installation and RPM/raw packaging with fixtures
	python3 "$(DELIVERY_ROOT)/tests/test_packaging_layout.py"
	python3 -m unittest discover -s "$(DELIVERY_ROOT)/tests" -p test_raw_packaging.py -k legacy_stage
