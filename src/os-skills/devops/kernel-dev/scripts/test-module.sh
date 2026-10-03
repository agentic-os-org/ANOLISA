#!/bin/bash
# test-module.sh - Automated kernel module testing script
# Usage: ./test-module.sh <module_name> [module_params...]
# Example: ./test-module.sh hello_module
#          ./test-module.sh param_module greeting="Welcome" repeat_count=3

set -e

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Parse arguments
MODULE_NAME="${1:?Usage: $0 <module_name> [module_params...]}"
shift
MODULE_PARAMS="$*"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASE_DIR="$(dirname "$SCRIPT_DIR")"

# Search for module file in examples directories
MODULE_FILE=""
for dir in "$BASE_DIR/examples/$MODULE_NAME" "$BASE_DIR/examples/hello_module" "$BASE_DIR/examples/param_module" "$BASE_DIR/examples/proc_module" "$BASE_DIR/examples/char_device" "$SCRIPT_DIR"; do
    if [ -f "$dir/${MODULE_NAME}.ko" ]; then
        MODULE_FILE="$dir/${MODULE_NAME}.ko"
        break
    fi
done

if [ -z "$MODULE_FILE" ]; then
    MODULE_FILE="${SCRIPT_DIR}/${MODULE_NAME}.ko"
fi

echo "========================================"
echo "  Kernel Module Test Suite"
echo "========================================"
echo ""
echo "Module: $MODULE_NAME"
echo "Path:   $MODULE_FILE"
if [ -n "$MODULE_PARAMS" ]; then
    echo "Params: $MODULE_PARAMS"
fi
echo ""

# Check if module file exists
if [ ! -f "$MODULE_FILE" ]; then
    echo -e "${RED}✗ Module file not found: $MODULE_FILE${NC}"
    echo "Please compile the module first: make"
    exit 1
fi
echo -e "${GREEN}✓${NC} Module file found"

# Check if running as root
if [ "$EUID" -ne 0 ]; then 
    echo -e "${YELLOW}⚠${NC} This script requires root privileges"
    echo "Please run with sudo: sudo $0 $MODULE_NAME"
    exit 1
fi

# Get module info
echo ""
echo -e "${BLUE}Step 1: Module Information${NC}"
modinfo "$MODULE_FILE"
echo ""

# Check if module is already loaded
echo -e "${BLUE}Step 2: Check Existing Module${NC}"
if lsmod | grep -q "^${MODULE_NAME}"; then
    echo -e "${YELLOW}⚠${NC} Module is already loaded, unloading..."
    rmmod "$MODULE_NAME" 2>/dev/null || true
    sleep 1
fi
echo -e "${GREEN}✓${NC} Module is not loaded"
echo ""

# Load module
echo -e "${BLUE}Step 3: Loading Module${NC}"
if [ -n "$MODULE_PARAMS" ]; then
    echo "Loading with parameters: $MODULE_PARAMS"
    insmod "$MODULE_FILE" $MODULE_PARAMS
else
    insmod "$MODULE_FILE"
fi
echo -e "${GREEN}✓${NC} Module loaded"
echo ""

# Verify module is loaded
echo -e "${BLUE}Step 4: Verify Module Loaded${NC}"
if lsmod | grep -q "^${MODULE_NAME}"; then
    echo -e "${GREEN}✓${NC} Module is loaded"
    lsmod | grep "^${MODULE_NAME}"
else
    echo -e "${RED}✗${NC} Module failed to load"
    exit 1
fi
echo ""

# Check for device or proc entries (if applicable)
echo -e "${BLUE}Step 5: Check Device/Proc Entries${NC}"
DEVICE_FILE="/dev/${MODULE_NAME}"
PROC_FILE="/proc/${MODULE_NAME}"

if [ -c "$DEVICE_FILE" ]; then
    echo -e "${GREEN}✓${NC} Character device found: $DEVICE_FILE"
    ls -l "$DEVICE_FILE"
elif [ -f "$PROC_FILE" ]; then
    echo -e "${GREEN}✓${NC} Proc entry found: $PROC_FILE"
    ls -l "$PROC_FILE"
else
    echo -e "${YELLOW}ℹ${NC} No device or proc entry (this may be normal)"
fi
echo ""

# View kernel logs
echo -e "${BLUE}Step 6: Kernel Logs (dmesg)${NC}"
dmesg | tail -10 | grep -i "$MODULE_NAME" || echo "No logs found"
echo ""

# Test duration
echo -e "${BLUE}Step 7: Test Duration${NC}"
echo "Module will remain loaded for 3 seconds..."
sleep 3
echo ""

# Unload module
echo -e "${BLUE}Step 8: Unloading Module${NC}"
rmmod "$MODULE_NAME"
echo -e "${GREEN}✓${NC} Module unloaded"
echo ""

# Verify module is unloaded
echo -e "${BLUE}Step 9: Verify Module Unloaded${NC}"
if lsmod | grep -q "^${MODULE_NAME}"; then
    echo -e "${RED}✗${NC} Module failed to unload"
    exit 1
else
    echo -e "${GREEN}✓${NC} Module is unloaded"
fi
echo ""

# Final kernel logs
echo -e "${BLUE}Step 10: Final Kernel Logs${NC}"
dmesg | tail -5 | grep -i "$MODULE_NAME" || echo "No logs found"
echo ""

# Summary
echo "========================================"
echo -e "  ${GREEN}Test Complete!${NC}"
echo "========================================"
echo ""
echo -e "${GREEN}✓${NC} All tests passed for $MODULE_NAME"
