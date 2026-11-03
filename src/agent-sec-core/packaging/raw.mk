# =============================================================================
# RAW PACKAGE
# =============================================================================

OUTPUT_DIR ?= $(BUILD_DIR)/raw
TARGET_OS ?= linux
TARGET_ARCH ?= $(shell uname -m)
RAW_CONTRACT ?= packaging/anolisa/component.toml
RAW_PYTHON_RUNTIME ?= $(BUILD_DIR)/python-runtime
RAW_PYTHON_ARCHIVE ?= $(BUILD_DIR)/downloads/cpython-3.11.6+20231002-x86_64-unknown-linux-gnu-install_only.tar.gz

.PHONY: prepare-raw-python
prepare-raw-python: build-all-v2 ## Stage the adapter-only bundled Python runtime
	./packaging/raw/prepare_python_runtime.sh \
		"$(abspath $(RAW_PYTHON_RUNTIME))" "$(abspath $(RAW_PYTHON_ARCHIVE))"

.PHONY: stage-raw
stage-raw: ## Stage the V2 raw payload under DESTDIR
	@BUILD_DIR="$(abspath $(BUILD_DIR))" \
		DESTDIR="$(DESTDIR)" \
		RAW_CONTRACT="$(abspath $(RAW_CONTRACT))" \
		TARGET_OS="$(TARGET_OS)" \
		TARGET_ARCH="$(TARGET_ARCH)" \
		./packaging/raw/package.sh stage

.PHONY: package-raw
package-raw: prepare-raw-python ## Build the deterministic V2 raw archive
	@BUILD_DIR="$(abspath $(BUILD_DIR))" \
		OUTPUT_DIR="$(abspath $(OUTPUT_DIR))" \
		RAW_CONTRACT="$(abspath $(RAW_CONTRACT))" \
		TARGET_OS="$(TARGET_OS)" \
		TARGET_ARCH="$(TARGET_ARCH)" \
		SOURCE_DATE_EPOCH="$(SOURCE_DATE_EPOCH)" \
		./packaging/raw/package.sh package

.PHONY: test-package-raw
test-package-raw: ## Verify the V2 raw package contract with fixtures
	./tests/packaging/test-package-raw.sh
