CARGO ?= cargo
CARGO_TARGET_DIR ?= $(COMPONENT_ROOT)/target
packaging_quote = '$(subst ','"'"',$(1))'

.PHONY: build
build:
	$(CARGO) build --release --locked \
		--manifest-path $(call packaging_quote,$(COMPONENT_ROOT)/Cargo.toml) \
		--target-dir $(call packaging_quote,$(CARGO_TARGET_DIR)) -p aw-cli --bin aw
