PREFIX ?= /usr/local
AWPRJ ?=
DESTDIR ?=
packaging_trim = $(if $(filter %/,$(1)),$(call packaging_trim,$(patsubst %/,%,$(1))),$(1))
packaging_prefix := $(call packaging_trim,$(PREFIX))
packaging_namespace := $(if $(AWPRJ),$(AWPRJ)/,)
BINDIR ?= $(packaging_prefix)/bin
DOCDIR ?= $(packaging_prefix)/share/$(packaging_namespace)doc/aw
BINARY ?= $(CARGO_TARGET_DIR)/release/aw
LICENSE_FILE ?= $(COMPONENT_ROOT)/../../LICENSE
DOC_FILES ?= $(LICENSE_FILE) $(COMPONENT_ROOT)/README.md $(COMPONENT_ROOT)/README_zh.md

.PHONY: install install-core check-install-inputs
check-install-inputs:
	@set -eu; \
	namespace=$(call packaging_quote,$(AWPRJ)); \
	case "$$namespace" in *[!a-zA-Z0-9_.-]*|.|..) echo 'AWPRJ must be empty or one namespace segment' >&2; exit 1;; esac; \
	for directory in $(call packaging_quote,$(PREFIX)) $(call packaging_quote,$(BINDIR)) $(call packaging_quote,$(DOCDIR)); do \
		case "$$directory" in /*) ;; *) echo "Expected an absolute installation path: $$directory" >&2; exit 1;; esac; \
		case "/$$directory/" in */../*|*/./*) echo "Expected a normalized installation path: $$directory" >&2; exit 1;; esac; \
	done; \
	root=$(call packaging_quote,$(DESTDIR)); \
	case "$$root" in ''|/*) ;; *) echo 'DESTDIR must be empty or absolute' >&2; exit 1;; esac; \
	case "/$$root/" in */../*|*/./*) echo 'DESTDIR must not contain dot segments' >&2; exit 1;; esac; \
	test -f $(call packaging_quote,$(BINARY)) && test -x $(call packaging_quote,$(BINARY)) || { echo 'Missing prebuilt AW executable; run make build first or set BINARY' >&2; exit 1; }; \
	for file in $(foreach file,$(DOC_FILES),$(call packaging_quote,$(file))); do \
		test -f "$$file" || { echo "Missing installation input: $$file" >&2; exit 1; }; \
	done

# File installation does not create an AW package database or activate a service.
install: install-core

install-core: check-install-inputs
	install -d $(call packaging_quote,$(DESTDIR)$(BINDIR)) $(call packaging_quote,$(DESTDIR)$(DOCDIR))
	install -m 0755 $(call packaging_quote,$(BINARY)) $(call packaging_quote,$(DESTDIR)$(BINDIR)/aw)
	install -m 0644 $(foreach file,$(DOC_FILES),$(call packaging_quote,$(file))) $(call packaging_quote,$(DESTDIR)$(DOCDIR)/)
