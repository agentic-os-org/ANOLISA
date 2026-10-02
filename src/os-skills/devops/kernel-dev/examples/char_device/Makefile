obj-m += char_device.o

KERNEL_DIR := /lib/modules/$(shell uname -r)/build
PWD := $(shell pwd)
MODULE_NAME := char_device
DEVICE_NAME := chardev

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
	@echo "=== Device Status ==="
	@if [ -c /dev/$(DEVICE_NAME) ]; then \
		echo "Device exists: /dev/$(DEVICE_NAME)"; \
		ls -l /dev/$(DEVICE_NAME); \
	else \
		echo "Device not found: /dev/$(DEVICE_NAME)"; \
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
	@echo "=== Testing Device Read ==="
	@echo "Reading from /dev/$(DEVICE_NAME):"
	@sudo cat /dev/$(DEVICE_NAME) || echo "Read test failed"
	@echo ""
	@echo "=== Testing Device Write ==="
	@echo "Writing to /dev/$(DEVICE_NAME):"
	@echo "Test message from user" | sudo tee /dev/$(DEVICE_NAME) > /dev/null
	@echo "Reading back:"
	@sudo cat /dev/$(DEVICE_NAME) || echo "Read back failed"
	@echo ""
	@make status

unload-test: status unload status

.PHONY: all check-deps modules clean rebuild install unload reload status info log test unload-test
