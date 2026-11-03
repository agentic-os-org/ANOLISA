# Included Python packaging implementation; public entrypoint is ../Makefile.
PYTHON_RUNTIME_DIR := python/tokenless
AGENTSCOPE_PACKAGE_DIR := python/agentscope
PYTHON_WHEEL_DIR := $(CURDIR)/target/wheels
PYTHON_RTK_PACKAGE_DIR := $(CURDIR)/$(PYTHON_RUNTIME_DIR)/python/anolisa_tokenless/_bin
PYTHON_RTK_BINARY ?= $(CURDIR)/third_party/rtk/target/release/rtk
MATURIN ?= uvx --from 'maturin>=1.9,<2.0' maturin
PYPROJECT_BUILD ?= uvx --from 'build>=1,<2' pyproject-build

# Build the abi3 extension with unwind semantics so a Rust panic cannot abort
# the embedding Python interpreter. The wheel remains platform-specific, but
# one build supports CPython 3.11 and later on that platform.
python-wheel:
	@echo "==> Building Tokenless Python wheel..."
	@if [ "$(PYTHON_RTK_BINARY)" = "$(CURDIR)/third_party/rtk/target/release/rtk" ]; then \
		just setup-rtk; \
		cargo build --release --locked --manifest-path third_party/rtk/Cargo.toml; \
	fi
	@test -x "$(PYTHON_RTK_BINARY)" || { \
		echo "RTK binary is missing or not executable: $(PYTHON_RTK_BINARY)" >&2; \
		exit 1; \
	}
	install -d -m 0755 $(PYTHON_WHEEL_DIR)
	rm -f $(PYTHON_WHEEL_DIR)/anolisa_tokenless-*.whl
	@set -e; \
		install -d -m 0755 $(PYTHON_RTK_PACKAGE_DIR); \
		install -p -m 0755 $(PYTHON_RTK_BINARY) $(PYTHON_RTK_PACKAGE_DIR)/rtk; \
		trap 'rm -f $(PYTHON_RTK_PACKAGE_DIR)/rtk' EXIT; \
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
