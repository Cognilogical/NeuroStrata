#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_PLUGIN_DIR="$HOME/.local/share/neurostrata/plugin"
OPENCODE_CONFIG="$HOME/.config/opencode/opencode.json"
GITHUB_REPO="your-username/neurostrata"

echo "Setting up the NeuroStrata OpenCode plugin..."
PLUGIN_DIR="$SCRIPT_DIR/neurostrata-plugin"

if [ -d "$PLUGIN_DIR/dist" ] && [ -f "$PLUGIN_DIR/package.json" ]; then
    echo "  -> Found local neurostrata-plugin source. Copying to $INSTALL_PLUGIN_DIR..."
    mkdir -p "$INSTALL_PLUGIN_DIR"
    cp -r "$PLUGIN_DIR/dist" "$INSTALL_PLUGIN_DIR/"
    cp "$PLUGIN_DIR/package.json" "$INSTALL_PLUGIN_DIR/"
else
    # Production Mode: Download pre-compiled plugin tarball from GitHub Releases
    PLUGIN_URL="https://github.com/$GITHUB_REPO/releases/latest/download/opencode-neurostrata.tgz"
    echo "  -> Downloading pre-compiled plugin from $PLUGIN_URL..."
    
    mkdir -p "$INSTALL_PLUGIN_DIR"
    if curl -f -L "$PLUGIN_URL" -o "$INSTALL_PLUGIN_DIR/plugin.tgz"; then
        tar -xzf "$INSTALL_PLUGIN_DIR/plugin.tgz" -C "$INSTALL_PLUGIN_DIR" --strip-components=1
        rm "$INSTALL_PLUGIN_DIR/plugin.tgz"
        echo "  -> Plugin downloaded and extracted successfully."
    else
        echo "  -> Error: Failed to download pre-compiled plugin tarball."
        echo "     If you are compiling from source, please run ./mcp/build.sh first."
        exit 1
    fi
fi

# Patch OpenCode configuration
echo "Patching OpenCode configuration..."
mkdir -p "$(dirname "$OPENCODE_CONFIG")"
if [ ! -f "$OPENCODE_CONFIG" ]; then
    echo "{}" > "$OPENCODE_CONFIG"
fi

if command -v jq &>/dev/null; then
    # Safely update JSON using jq
    jq '.mcp |= (. // {}) | .mcp.neurostrata = {"type": "local", "command": ["'"$HOME"'/.local/bin/neurostrata-mcp"]} | .plugin |= (. // []) | if (.plugin | index("'"$INSTALL_PLUGIN_DIR"'") | not) then .plugin = (.plugin | map(select(. != "opencode-neurostrata"))) + ["'"$INSTALL_PLUGIN_DIR"'"] else . end' "$OPENCODE_CONFIG" > "${OPENCODE_CONFIG}.tmp" && mv "${OPENCODE_CONFIG}.tmp" "$OPENCODE_CONFIG"
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
data.mcp.neurostrata = { type: 'local', command: [path.join(process.env.HOME, '.local/bin/neurostrata-mcp')] };
data.plugin = data.plugin || [];
// Remove legacy global npm link name if it exists
data.plugin = data.plugin.filter(p => p !== 'opencode-neurostrata');
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
