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
OUTPUT_DIR="$PROJECT_PATH/.neurostrata/docs"

echo "Bootstrapping project: $PROJECT_NAME"
echo "Source path: $PROJECT_PATH"
echo "Output directory: $OUTPUT_DIR"

mkdir -p "$OUTPUT_DIR"

echo "Running deepwiki-rs on $PROJECT_PATH..."
# Run deepwiki-rs to generate the project documentation in the project folder
deepwiki-rs -p "$PROJECT_PATH" -o "$OUTPUT_DIR"

NEUROVAULT_DIR="$HOME/Documents/NeuroVault"
echo "Symlinking project to NeuroVault ($NEUROVAULT_DIR/$PROJECT_NAME)..."
mkdir -p "$NEUROVAULT_DIR"
ln -sfn "$PROJECT_PATH" "$NEUROVAULT_DIR/$PROJECT_NAME"

echo "==============================================="
echo "  Bootstrapping Complete!"
echo "  Project documentation has been generated in:"
echo "  $OUTPUT_DIR"
echo ""
echo "  The project has also been symlinked into your"
echo "  NeuroVault at: $NEUROVAULT_DIR/$PROJECT_NAME"
echo ""
echo "  You can now review these documents in Obsidian and"
echo "  use NeuroStrata agents to ingest key architectural"
echo "  rules directly into the 3-Tier Memory."
echo "==============================================="
