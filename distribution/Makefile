PYTHON ?= python3
CARGO ?= cargo
VERSION ?= 0.1.0
COMPONENTS ?= core
BACKENDS ?= sec-core tokenless
OUTPUT ?= $(abspath $(CURDIR)/../target/aw-packages)

export CARGO

.PHONY: aw-packages aw-integration-backends test-aw-packages

aw-packages:
	PYTHON="$(PYTHON)" ./packaging/aw/build.sh \
		--version "$(VERSION)" --components "$(COMPONENTS)" --output "$(OUTPUT)"

aw-integration-backends:
	PYTHON="$(PYTHON)" ./packaging/aw/build-integration.sh --backends "$(BACKENDS)"

test-aw-packages:
	$(PYTHON) -B -m unittest discover -s packaging/aw/tests -v
