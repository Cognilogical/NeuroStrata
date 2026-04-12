#!/bin/bash
set -e

PROJECT_DIR="/home/kenton/Documents/strata"
BIN_DIR="$PROJECT_DIR/bin"
MCP_SRC_DIR="$PROJECT_DIR/mcp"

echo "Building Strata Go MCP Server from source..."
cd "$MCP_SRC_DIR"
go mod tidy
mkdir -p "$BIN_DIR"

# Build for current architecture
go build -o "$BIN_DIR/strata-mcp" .
echo "Successfully built to $BIN_DIR/strata-mcp"
