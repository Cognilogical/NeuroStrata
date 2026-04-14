#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname \"${BASH_SOURCE[0]}\")" && pwd)"
INSTALL_PLUGIN_DIR="$HOME/.local/share/strata/plugin"
OPENCLAW_CONFIG="$HOME/.config/openclaw/openclaw.json"
GITHUB_REPO="your-username/strata"

echo "Setting up the Strata OpenClaw plugin..."
PLUGIN_DIR="$SCRIPT_DIR/strata-plugin"

if [ -d "$PLUGIN_DIR/dist" ] && [ -f "$PLUGIN_DIR/package.json" ]; then
    echo "  -> Found local strata-plugin source. Copying to $INSTALL_PLUGIN_DIR..."
    mkdir -p "$INSTALL_PLUGIN_DIR"
    cp -r "$PLUGIN_DIR/dist" "$INSTALL_PLUGIN_DIR/"
    cp "$PLUGIN_DIR/package.json" "$INSTALL_PLUGIN_DIR/"
else
    PLUGIN_URL="https://github.com/$GITHUB_REPO/releases/latest/download/openclaw-strata.tgz"
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

# Patch OpenClaw configuration
echo "Patching OpenClaw configuration..."
mkdir -p "$(dirname "$OPENCLAW_CONFIG")"
if [ ! -f "$OPENCLAW_CONFIG" ]; then
    echo "{}" > "$OPENCLAW_CONFIG"
fi

if command -v jq &>/dev/null; then
    jq '.plugin |= (. // []) | if (.plugin | index("$INSTALL_PLUGIN_DIR") | not) then .plugin = (.plugin | map(select(. != "openclaw-strata"))) + ["$INSTALL_PLUGIN_DIR"] else . end' "$OPENCLAW_CONFIG" > "${OPENCLAW_CONFIG}.tmp" && mv "${OPENCLAW_CONFIG}.tmp" "$OPENCLAW_CONFIG"
    echo "  -> OpenClaw configuration updated successfully using jq."
elif command -v node &>/dev/null; then
    node -e "
    const fs = require('fs');
    const filepath = process.argv[1];
    const pluginPath = process.argv[2];
    let data = JSON.parse(fs.readFileSync(filepath, 'utf8'));
    data.plugin = data.plugin || [];
    data.plugin = data.plugin.filter(p => p !== 'openclaw-strata');
    if (!data.plugin.includes(pluginPath)) {
        data.plugin.push(pluginPath);
    }
    fs.writeFileSync(filepath, JSON.stringify(data, null, 2));
    " "$OPENCLAW_CONFIG" "$INSTALL_PLUGIN_DIR"
    echo "  -> OpenClaw configuration updated successfully using Node.js fallback."
else
    echo "  -> Notice: Neither 'jq' nor 'node' were found."
    echo "     Please manually add the plugin path to $OPENCLAW_CONFIG."
fi