#!/usr/bin/env bash
set -e

echo "==============================================="
echo "  NeuroStrata: Project Bootstrapper           "
echo "==============================================="

if ! command -v deepwiki-rs &> /dev/null; then
    echo "deepwiki-rs is not installed. Please run install.sh first or run:"
    echo "cargo install deepwiki-rs"
    exit 1
fi

if [ -z "$1" ]; then
    echo "Usage: ./scripts/bootstrap.sh <path-to-project>"
    echo "Example: ./scripts/bootstrap.sh /home/user/my-project"
    exit 1
fi

PROJECT_PATH="$1"
PROJECT_NAME=$(basename "$PROJECT_PATH")
OUTPUT_DIR="$HOME/Documents/NeuroVault/$PROJECT_NAME"

echo "Bootstrapping project: $PROJECT_NAME"
echo "Source path: $PROJECT_PATH"
echo "Output directory: $OUTPUT_DIR"

mkdir -p "$OUTPUT_DIR"

echo "Running deepwiki-rs on $PROJECT_PATH..."
# Run deepwiki-rs to generate the project documentation in the Obsidian vault
deepwiki-rs -p "$PROJECT_PATH" -o "$OUTPUT_DIR"

echo "==============================================="
echo "  Bootstrapping Complete!"
echo "  Project documentation has been generated in:"
echo "  $OUTPUT_DIR"
echo ""
echo "  You can now open this folder in Obsidian and use"
echo "  the obsidian-neurostrata plugin to curate these"
echo "  architectural documents directly into the 3-Tier Memory."
echo "==============================================="
