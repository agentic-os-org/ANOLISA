# Makefile for ws-ckpt

.DEFAULT_GOAL := build

include packaging/install.mk
include packaging/delivery.mk

.PHONY: build install install-legacy uninstall clean test test-skill test-openclaw-scripts test-openclaw-compat test-openclaw-install test-openclaw-uninstall package-raw test-package-raw help


build: build-core build-plugin

build-core:
	cd src && cargo build --release --workspace

build-plugin:
	cd src/plugins/openclaw && npm install --ignore-scripts && npm run build


clean:
	cd src && cargo clean

test:
	cd src && cargo test --workspace
	$(MAKE) test-openclaw-scripts test-skill

test-skill:
	python3 -B tests/test_aw_snapshot_control.py

test-openclaw-scripts: test-openclaw-compat test-openclaw-install test-openclaw-uninstall

test-openclaw-compat:
	bash tests/openclaw-compat.sh

test-openclaw-install:
	bash tests/openclaw-install.sh

test-openclaw-uninstall:
	bash tests/openclaw-uninstall.sh

.PHONY: rpm srpm
rpm:
	packaging/rpm/build.sh

srpm:
	packaging/rpm/build.sh --srpm

package-raw:
	packaging/raw/package.sh package

test-package-raw:
	python3 -m unittest discover -s tests -p test_raw_packaging.py -v

help:
	@echo "Targets: build install install-core install-plugins install-skills install-legacy uninstall clean test test-skill test-openclaw-scripts package-raw test-package-raw layout test-packaging"
	@echo "Variables:"
	@echo "  INSTALL_PROFILE=user|system"
	@echo "  DESTDIR= optional temporary installation root"
	@echo "  PREFIX=/usr/local AWPRJ=anolisa install prefix and namespace"
