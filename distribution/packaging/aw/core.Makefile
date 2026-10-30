PREFIX ?= /usr/local
AWPRJ ?=
DESTDIR ?=
COMPONENT_ROOT := $(CURDIR)
packaging_quote = '$(subst ','"'"',$(1))'
BINARY ?= $(CURDIR)/bin/aw
LICENSE_FILE ?= $(CURDIR)/share/doc/aw/LICENSE
DOC_FILES ?= $(LICENSE_FILE) $(CURDIR)/README.md $(CURDIR)/README_zh.md $(CURDIR)/share/doc/aw/NOTICE $(CURDIR)/share/doc/aw/source.json

include packaging/install.mk
