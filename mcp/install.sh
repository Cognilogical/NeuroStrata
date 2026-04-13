#!/bin/bash
set -e

# ==============================================================================
# Configuration
# Change this to your actual GitHub username and repository when publishing
GITHUB_REPO="your-username/strata"
# ==============================================================================

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

if [ "$ARCH" = "x86_64" ]; then
    ARCH="amd64"
elif [ "$ARCH" = "aarch64" ] || [ "$ARCH" = "arm64" ]; then
    ARCH="arm64"
fi

if [[ "$OS" == "mingw"* ]] || [[ "$OS" == "msys"* ]] || [[ "$OS" == "cygwin"* ]]; then
    OS="windows"
    BINARY_NAME="strata-mcp-windows-amd64.exe"
else
    BINARY_NAME="strata-mcp-${OS}-${ARCH}"
fi

INSTALL_DIR="$HOME/.local/share/strata/bin"
mkdir -p "$INSTALL_DIR"
DEST_BIN="$INSTALL_DIR/strata-mcp"

echo "Installing Strata Go MCP Server ($BINARY_NAME)..."

# 1. Detect if we are installing from source (development mode)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCAL_BIN="$SCRIPT_DIR/../bin/strata-mcp"
LOCAL_SKILL_DIR="$SCRIPT_DIR/../.agents/skills/strata"

if [ -f "$LOCAL_BIN" ]; then
    echo "Found local compiled binary, copying to $DEST_BIN..."
    cp "$LOCAL_BIN" "$DEST_BIN"
    chmod +x "$DEST_BIN"
else
    # 2. Production Mode: Download pre-compiled binary from GitHub Releases
    DOWNLOAD_URL="https://github.com/$GITHUB_REPO/releases/latest/download/$BINARY_NAME"
    echo "Downloading pre-compiled binary from $DOWNLOAD_URL..."
    
    if curl -f -L "$DOWNLOAD_URL" -o "$DEST_BIN"; then
        echo "Download successful."
        chmod +x "$DEST_BIN"
    else
        echo "Error: Failed to download pre-compiled binary ($BINARY_NAME)."
        echo "If you are compiling from source, please run ./build.sh first before running ./install.sh."
        exit 1
    fi
fi

echo "Setting up symlinks..."
# Symlink binary to ~/.local/bin
mkdir -p "$HOME/.local/bin"
ln -sf "$DEST_BIN" "$HOME/.local/bin/strata-mcp"
echo "  -> Linked $DEST_BIN to ~/.local/bin/strata-mcp"

# Symlink skill to ~/.agents/skills/strata
mkdir -p "$HOME/.agents/skills/strata"
if [ -d "$LOCAL_SKILL_DIR" ]; then
    # We are in the source tree, symlink the local directory
    ln -sfn "$LOCAL_SKILL_DIR" "$HOME/.agents/skills/strata"
    echo "  -> Linked local $LOCAL_SKILL_DIR to ~/.agents/skills/strata"
else
    # We are downloading raw, fetch the SKILL.md from GitHub
    curl -f -s -o "$HOME/.agents/skills/strata/SKILL.md" "https://raw.githubusercontent.com/$GITHUB_REPO/main/.agents/skills/strata/SKILL.md"
    echo "  -> Downloaded SKILL.md directly from GitHub to ~/.agents/skills/strata"
fi

# Config setup
CONFIG_DIR="$HOME/.config/strata"
CONFIG_FILE="$CONFIG_DIR/config.json"
echo "Setting up configuration at $CONFIG_FILE..."
mkdir -p "$CONFIG_DIR"

if [ ! -f "$CONFIG_FILE" ]; then
cat << 'JSON' > "$CONFIG_FILE"
{
  "embedder_url": "http://localhost:8004/v1/embeddings",
  "embedder_model": "nomic-embed-text-v1.5.f16.gguf",
  "embedder_api_key": "sk-local",
  "qdrant_url": "http://localhost:6333",
  "qdrant_collection": "strata",
  "http_port": "8005"
}
JSON
  echo "Default configuration created."
else
  echo "Configuration already exists."
fi

OPENCODE_CONFIG="$HOME/.config/opencode/opencode.json"

# Build and link the OpenCode TypeScript plugin
echo "Setting up the Strata OpenCode plugin..."
PLUGIN_DIR="$SCRIPT_DIR/../strata-plugin"
INSTALL_PLUGIN_DIR="$HOME/.local/share/strata/plugin"

if [ -d "$PLUGIN_DIR/dist" ] && [ -f "$PLUGIN_DIR/package.json" ]; then
    echo "  -> Found local strata-plugin source. Copying to $INSTALL_PLUGIN_DIR..."
    mkdir -p "$INSTALL_PLUGIN_DIR"
    cp -r "$PLUGIN_DIR/dist" "$INSTALL_PLUGIN_DIR/"
    cp "$PLUGIN_DIR/package.json" "$INSTALL_PLUGIN_DIR/"
else
    # 3. Production Mode: Download pre-compiled plugin tarball from GitHub Releases
    PLUGIN_URL="https://github.com/$GITHUB_REPO/releases/latest/download/opencode-strata.tgz"
    echo "  -> Downloading pre-compiled plugin from $PLUGIN_URL..."
    
    mkdir -p "$INSTALL_PLUGIN_DIR"
    if curl -f -L "$PLUGIN_URL" -o "$INSTALL_PLUGIN_DIR/plugin.tgz"; then
        tar -xzf "$INSTALL_PLUGIN_DIR/plugin.tgz" -C "$INSTALL_PLUGIN_DIR" --strip-components=1
        rm "$INSTALL_PLUGIN_DIR/plugin.tgz"
        echo "  -> Plugin downloaded and extracted successfully."
    else
        echo "  -> Error: Failed to download pre-compiled plugin tarball."
        echo "     If you are compiling from source, please run ./build.sh first."
        exit 1
    fi
fi

# Patch OpenCode configuration
echo "Patching OpenCode configuration..."
if [ -f "$OPENCODE_CONFIG" ]; then
    if command -v jq &>/dev/null; then
        # Safely update JSON using jq
        jq '.mcp |= (. // {}) | .mcp.strata = {"type": "local", "command": ["'"$HOME"'/.local/bin/strata-mcp"]} | .plugin |= (. // []) | if (.plugin | index("'"$INSTALL_PLUGIN_DIR"'") | not) then .plugin = (.plugin | map(select(. != "opencode-strata"))) + ["'"$INSTALL_PLUGIN_DIR"'"] else . end' "$OPENCODE_CONFIG" > "${OPENCODE_CONFIG}.tmp" && mv "${OPENCODE_CONFIG}.tmp" "$OPENCODE_CONFIG"
        echo "  -> OpenCode configuration updated successfully using jq."
    elif command -v node &>/dev/null; then
        # Fallback to Node.js
        node -e "
const fs = require('fs');
const path = require('path');
const filepath = process.argv[1];
const pluginPath = process.argv[2];
let data = JSON.parse(fs.readFileSync(filepath, 'utf8'));
data.mcp = data.mcp || {};
data.mcp.strata = { type: 'local', command: [path.join(process.env.HOME, '.local/bin/strata-mcp')] };
data.plugin = data.plugin || [];
// Remove legacy global npm link name if it exists
data.plugin = data.plugin.filter(p => p !== 'opencode-strata');
if (!data.plugin.includes(pluginPath)) {
    data.plugin.push(pluginPath);
}
fs.writeFileSync(filepath, JSON.stringify(data, null, 2));
" "$OPENCODE_CONFIG" "$INSTALL_PLUGIN_DIR"
        echo "  -> OpenCode configuration updated successfully using Node.js fallback."
    else
        echo "  -> Notice: Neither 'jq' nor 'node' were found."
        echo "     Please manually add the MCP configuration to $OPENCODE_CONFIG."
    fi
else
    echo "  -> Warning: $OPENCODE_CONFIG not found."
fi

echo "Installation Complete!"
