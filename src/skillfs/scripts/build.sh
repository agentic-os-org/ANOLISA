#!/bin/bash
# Build SkillFS project

set -e

echo "=== Building SkillFS ==="
cd "$(dirname "$0")/.."

echo "Building debug version..."
cargo build --workspace

echo ""
echo "Build complete! Binary location:"
echo "  Debug: target/debug/skillfs"
echo ""
echo "To build release version, run:"
echo "  cargo build --release"
