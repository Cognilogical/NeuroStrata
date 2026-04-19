#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_PLUGIN_DIR="$HOME/.local/share/neurostrata/plugin"
OPENCODE_CONFIG="$HOME/.config/opencode/opencode.json"

echo "Setting up the NeuroStrata OpenCode plugin..."
PLUGIN_DIR="$SCRIPT_DIR/neurostrata-plugin"

if [ -f "$PLUGIN_DIR/package.json" ]; then
    echo "  -> Building local neurostrata-plugin source..."
    cd "$PLUGIN_DIR"
    npm install
    npm run build
    cd -
    
    echo "  -> Copying to $INSTALL_PLUGIN_DIR..."
    mkdir -p "$INSTALL_PLUGIN_DIR"
    cp -r "$PLUGIN_DIR/dist" "$INSTALL_PLUGIN_DIR/"
    cp "$PLUGIN_DIR/package.json" "$INSTALL_PLUGIN_DIR/"
else
    echo "  -> Error: Local neurostrata-plugin not found at $PLUGIN_DIR"
    exit 1
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
