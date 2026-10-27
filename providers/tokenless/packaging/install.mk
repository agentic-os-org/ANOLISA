PREFIX ?= /usr/local
AWPRJ ?=
DESTDIR ?=
packaging_trim = $(if $(filter %/,$(1)),$(call packaging_trim,$(patsubst %/,%,$(1))),$(1))
packaging_prefix := $(call packaging_trim,$(PREFIX))
packaging_namespace := $(if $(AWPRJ),$(AWPRJ)/,)
LIBEXECDIR ?= $(packaging_prefix)/libexec/$(packaging_namespace)aw/providers/tokenless
DATADIR ?= $(packaging_prefix)/share/$(packaging_namespace)aw/providers/tokenless
DOCDIR ?= $(packaging_prefix)/share/$(packaging_namespace)doc/aw-provider-tokenless
BINARY ?= $(CARGO_TARGET_DIR)/release/aw-provider-tokenless
LICENSE_FILE ?= $(COMPONENT_ROOT)/../../LICENSE

.PHONY: install check-install-inputs
check-install-inputs:
	@set -eu; \
	namespace=$(call packaging_quote,$(AWPRJ)); \
	case "$$namespace" in *[!a-zA-Z0-9_.-]*|.|..) echo 'AWPRJ must be empty or one namespace segment' >&2; exit 1;; esac; \
	for directory in $(call packaging_quote,$(PREFIX)) $(call packaging_quote,$(LIBEXECDIR)) $(call packaging_quote,$(DATADIR)) $(call packaging_quote,$(DOCDIR)); do \
		case "$$directory" in /*) ;; *) echo "Expected an absolute installation path: $$directory" >&2; exit 1;; esac; \
		case "/$$directory/" in */../*|*/./*) echo "Expected a normalized installation path: $$directory" >&2; exit 1;; esac; \
	done; \
	root=$(call packaging_quote,$(DESTDIR)); \
	case "$$root" in ''|/*) ;; *) echo 'DESTDIR must be empty or absolute' >&2; exit 1;; esac; \
	case "/$$root/" in */../*|*/./*) echo 'DESTDIR must not contain dot segments' >&2; exit 1;; esac; \
	test -f $(call packaging_quote,$(BINARY)) && test -x $(call packaging_quote,$(BINARY)) || { echo 'Missing prebuilt Provider executable; run make build first or set BINARY' >&2; exit 1; }; \
	for file in $(call packaging_quote,$(LICENSE_FILE)) $(call packaging_quote,$(COMPONENT_ROOT)/README.md) $(call packaging_quote,$(COMPONENT_ROOT)/README_zh.md) $(call packaging_quote,$(COMPONENT_ROOT)/examples/aw.yaml); do \
		test -f "$$file" || { echo "Missing installation input: $$file" >&2; exit 1; }; \
	done

# Tokenless and RTK remain independently installed backend executables.
install: check-install-inputs
	install -d $(call packaging_quote,$(DESTDIR)$(LIBEXECDIR)) $(call packaging_quote,$(DESTDIR)$(DATADIR)/examples) $(call packaging_quote,$(DESTDIR)$(DOCDIR))
	install -m 0755 $(call packaging_quote,$(BINARY)) $(call packaging_quote,$(DESTDIR)$(LIBEXECDIR)/aw-provider-tokenless)
	install -m 0644 $(call packaging_quote,$(COMPONENT_ROOT)/examples/aw.yaml) $(call packaging_quote,$(DESTDIR)$(DATADIR)/examples/)
	install -m 0644 $(call packaging_quote,$(LICENSE_FILE)) $(call packaging_quote,$(DESTDIR)$(DOCDIR)/LICENSE)
	install -m 0644 $(call packaging_quote,$(COMPONENT_ROOT)/README.md) $(call packaging_quote,$(COMPONENT_ROOT)/README_zh.md) $(call packaging_quote,$(DESTDIR)$(DOCDIR)/)
