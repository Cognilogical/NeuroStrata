#!/bin/bash
set -e

PROJECT_DIR="/home/kenton/Documents/strata"
MCP_SRC_DIR="$PROJECT_DIR/mcp"
DIST_DIR="$PROJECT_DIR/dist"

echo "Building cross-platform fat binaries for Strata MCP..."
cd "$MCP_SRC_DIR"
go mod tidy
mkdir -p "$DIST_DIR"

# Linux
echo "Building Linux (amd64)..."
GOOS=linux GOARCH=amd64 go build -o "$DIST_DIR/strata-mcp-linux-amd64" .
echo "Building Linux (arm64)..."
GOOS=linux GOARCH=arm64 go build -o "$DIST_DIR/strata-mcp-linux-arm64" .

# macOS (Darwin)
echo "Building macOS (amd64)..."
GOOS=darwin GOARCH=amd64 go build -o "$DIST_DIR/strata-mcp-darwin-amd64" .
echo "Building macOS (arm64/Apple Silicon)..."
GOOS=darwin GOARCH=arm64 go build -o "$DIST_DIR/strata-mcp-darwin-arm64" .

# Windows
echo "Building Windows (amd64)..."
GOOS=windows GOARCH=amd64 go build -o "$DIST_DIR/strata-mcp-windows-amd64.exe" .

echo "All binaries successfully compiled to $DIST_DIR/"
echo "You can now upload these files to your GitHub Releases page!"
