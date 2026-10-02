obj-m += proc_module.o

KERNEL_DIR := /lib/modules/$(shell uname -r)/build
PWD := $(shell pwd)
MODULE_NAME := proc_module
PROC_NAME := hello_proc

ifeq ($(wildcard $(KERNEL_DIR)),)
$(error Kernel build directory not found: $(KERNEL_DIR). Please run: sudo yum install kernel-devel-$(shell uname -r))
endif

all: check-deps modules

check-deps:
	@if [ ! -d "$(KERNEL_DIR)" ]; then \
		echo "Error: Kernel build directory not found: $(KERNEL_DIR)"; \
		echo "Please run: sudo yum install kernel-devel-$(shell uname -r)"; \
		exit 1; \
	fi
	@echo "✓ Kernel build directory: $(KERNEL_DIR)"

modules:
	make -C $(KERNEL_DIR) M=$(PWD) modules

clean:
	make -C $(KERNEL_DIR) M=$(PWD) clean

rebuild: clean all

install:
	sudo insmod $(MODULE_NAME).ko

unload:
	sudo rmmod $(MODULE_NAME) || true

reload: unload install

status:
	@echo "=== Module Status ==="
	@if lsmod | grep -q $(MODULE_NAME); then \
		echo "Module is loaded"; \
		lsmod | grep $(MODULE_NAME); \
	else \
		echo "Module is not loaded"; \
	fi
	@echo ""
	@echo "=== Proc Entry Status ==="
	@if [ -f /proc/$(PROC_NAME) ]; then \
		echo "Proc entry exists: /proc/$(PROC_NAME)"; \
		ls -l /proc/$(PROC_NAME); \
	else \
		echo "Proc entry not found: /proc/$(PROC_NAME)"; \
	fi
	@echo ""
	@echo "=== Recent dmesg logs ==="
	@dmesg | tail -10 | grep -i $(MODULE_NAME) || echo "No recent logs found"

info:
	@echo "=== Module Information ==="
	modinfo $(MODULE_NAME).ko 2>/dev/null || echo "Module not compiled yet"

log:
	@echo "=== Kernel Logs ==="
	@dmesg | tail -20 | grep -i $(MODULE_NAME) || echo "No logs found"

test: all install status
	@echo ""
	@echo "=== Testing /proc Interface ==="
	@echo "Reading /proc/$(PROC_NAME) (1st read):"
	@cat /proc/$(PROC_NAME)
	@echo ""
	@echo "Reading /proc/$(PROC_NAME) (2nd read):"
	@cat /proc/$(PROC_NAME)
	@echo ""
	@make status

unload-test: status unload status

.PHONY: all check-deps modules clean rebuild install unload reload status info log test unload-test
