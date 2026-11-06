COMPONENT_ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
.DEFAULT_GOAL := build

include $(COMPONENT_ROOT)/packaging/build.mk
include $(COMPONENT_ROOT)/packaging/install.mk
