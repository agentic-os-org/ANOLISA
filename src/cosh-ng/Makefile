# Public component build and delivery entrypoint.
.DEFAULT_GOAL := build
.PHONY: build help
build:
	cargo build --workspace --release

DELIVERY_COMPONENT_ROOT := $(abspath $(dir $(firstword $(MAKEFILE_LIST))))
delivery_quote = '$(subst ','"'"',$(1))'
# Forward explicit paths; the installer resolves and validates layout defaults.
DELIVERY_INPUTS := AWPRJ PREFIX INSTALL_PROFILE BINDIR LIBDIR LIBEXECDIR DATADIR SKILLDIR UNITDIR BIN_DIR RTK_BIN DESTDIR
DELIVERY_ENV = env $(foreach name,$(DELIVERY_INPUTS) COMPONENT_ROOT,-u $(name)) \
	$(call delivery_quote,COMPONENT_ROOT=$(DELIVERY_COMPONENT_ROOT)) \
	$(foreach name,$(DELIVERY_INPUTS),$(if $(filter command line environment,$(origin $(name))),$(call delivery_quote,$(name)=$($(name)))))

.PHONY: layout install test-packaging
layout:
	@$(DELIVERY_ENV) bash "$(DELIVERY_COMPONENT_ROOT)/packaging/install.sh" --print-layout

install:
	@$(DELIVERY_ENV) bash "$(DELIVERY_COMPONENT_ROOT)/packaging/install.sh"

test-packaging:
	@bash "$(DELIVERY_COMPONENT_ROOT)/tests/test-system-install.sh"
	@python3 "$(DELIVERY_COMPONENT_ROOT)/tests/test_rpm_split.py"

help:
	@printf '%s\n' 'Targets: build install layout test-packaging' 'install copies prebuilt files; UID selects defaults; override PREFIX AWPRJ DESTDIR'
