# Included Python packaging implementation; public entrypoint is ../Makefile.
PYTHON_RUNTIME_DIR := python/tokenless
AGENTSCOPE_PACKAGE_DIR := python/agentscope
PYTHON_WHEEL_DIR := $(CURDIR)/target/wheels
PYTHON_BINARY_PACKAGE_DIR := $(CURDIR)/$(PYTHON_RUNTIME_DIR)/python/anolisa_tokenless/_bin
PYTHON_TOKENLESS_BINARY ?= $(CURDIR)/target/release/tokenless
MATURIN ?= uvx --from 'maturin>=1.9,<2.0' maturin
PYPROJECT_BUILD ?= uvx --from 'build>=1,<2' pyproject-build

# Build the abi3 extension with unwind semantics so a Rust panic cannot abort
# the embedding Python interpreter. The wheel remains platform-specific, but
# one build supports CPython 3.11 and later on that platform. RTK's source is
# set up even when PYTHON_TOKENLESS_BINARY names a prebuilt executable, because
# the PyO3 runtime links RTK as well.
python-wheel:
	@echo "==> Building Tokenless Python wheel..."
	just setup-rtk
	@if [ "$(PYTHON_TOKENLESS_BINARY)" = "$(CURDIR)/target/release/tokenless" ]; then \
		cargo build --release --locked -p tokenless-cli; \
	fi
	@test -x "$(PYTHON_TOKENLESS_BINARY)" || { \
		echo "Tokenless binary is missing or not executable: $(PYTHON_TOKENLESS_BINARY)" >&2; \
		exit 1; \
	}
	install -d -m 0755 $(PYTHON_WHEEL_DIR)
	rm -f $(PYTHON_WHEEL_DIR)/anolisa_tokenless-*.whl
	@set -e; \
		install -d -m 0755 $(PYTHON_BINARY_PACKAGE_DIR); \
		install -p -m 0755 $(PYTHON_TOKENLESS_BINARY) $(PYTHON_BINARY_PACKAGE_DIR)/tokenless; \
		trap 'rm -f $(PYTHON_BINARY_PACKAGE_DIR)/tokenless' EXIT; \
		cd $(PYTHON_RUNTIME_DIR); \
		$(MATURIN) build --profile python-release --interpreter python3 \
			--compatibility pypi --auditwheel check \
			--out $(PYTHON_WHEEL_DIR)

agentscope-wheel: stamp-python-packages
	@echo "==> Building Tokenless AgentScope integration wheel..."
	install -d -m 0755 $(PYTHON_WHEEL_DIR)
	rm -f $(PYTHON_WHEEL_DIR)/anolisa_tokenless_agentscope-*.whl
	$(PYPROJECT_BUILD) --wheel --outdir $(PYTHON_WHEEL_DIR) \
		$(AGENTSCOPE_PACKAGE_DIR)
