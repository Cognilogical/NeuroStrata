#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$SCRIPT_DIR/.."
BIN_DIR="$ROOT_DIR/bin"
DIST_DIR="$ROOT_DIR/dist"

mkdir -p "$BIN_DIR"
mkdir -p "$DIST_DIR"

echo "Building Strata Go MCP Server..."

PLATFORMS=("linux/amd64" "linux/arm64" "darwin/amd64" "darwin/arm64" "windows/amd64")

cd "$SCRIPT_DIR"
for PLATFORM in "${PLATFORMS[@]}"; do
    OS=${PLATFORM%/*}
    ARCH=${PLATFORM#*/}
    
    OUTPUT_NAME="strata-mcp-${OS}-${ARCH}"
    if [ "$OS" = "windows" ]; then
        OUTPUT_NAME="${OUTPUT_NAME}.exe"
    fi
    
    echo "  -> Compiling $OUTPUT_NAME..."
    # Use wildcard to compile all go files
    GOOS=$OS GOARCH=$ARCH go build -ldflags="-s -w" -o "$BIN_DIR/$OUTPUT_NAME" *.go
done

echo "Building Strata OpenCode Plugin..."
cd "$ROOT_DIR/plugins/opencode/strata-plugin"

if ! command -v npm &> /dev/null; then
    echo "Warning: npm is required to build the release plugin package, but it is not installed. Skipping plugin package."
    exit 0
fi

npm install
npm run build

echo "Packaging Strata OpenCode Plugin..."
PLUGIN_TARBALL="$DIST_DIR/opencode-strata.tgz"
npm pack --pack-destination "$DIST_DIR"
mv "$DIST_DIR"/opencode-strata-*.tgz "$PLUGIN_TARBALL"

echo "====================================="
echo "✅ Build Complete!"
echo "Binaries are in: $BIN_DIR/"
echo "Plugin Tarball is in: $DIST_DIR/"
echo "====================================="
