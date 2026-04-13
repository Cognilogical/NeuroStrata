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

# Patch OpenCode configuration
echo "Patching OpenCode configuration..."
# Patch AGENTS.md configuration
echo "Patching global OpenCode AGENTS.md configuration..."
AGENTS_MD="$HOME/.config/opencode/AGENTS.md"
mkdir -p "$(dirname "$AGENTS_MD")"

STRATA_BLOCK_HEADER="## 🧠 Strata Memory & Sessions"
STRATA_BLOCK_CONTENT="ALWAYS proactively invoke the \`strata\` skill at the very beginning of a new project or conversation. You MUST execute the Strata Startup Protocol (checking \`.sessions/\` and asking the user to start/resume a session) before writing any code or making architectural decisions. Rules retrieved from the global context are non-negotiable and MUST be followed."

if [ -f "$AGENTS_MD" ] && grep -q "$STRATA_BLOCK_HEADER" "$AGENTS_MD"; then
    echo "  -> Strata block already exists in $AGENTS_MD. Updating content..."
    # A bit complex to replace multiple lines in bash easily without perl/awk. 
    # The safest approach is to just replace the line directly below the header if we assume it's one paragraph, 
    # OR we can just delete from header to next double newline and re-append.
    # Given the simplicity, we'll use awk to filter out the old block and append the new one.
    awk -v header="$STRATA_BLOCK_HEADER" '
    BEGIN { skip = 0 }
    $0 ~ header { skip = 1; next }
    skip == 1 && /^## / { skip = 0 } # Stop skipping if we hit the next header
    skip == 1 && /^$/ { skip = 0 }   # Or if we hit a blank line
    skip == 0 { print }
    ' "$AGENTS_MD" > "${AGENTS_MD}.tmp"
    
    # Append the fresh block
    echo -e "\n$STRATA_BLOCK_HEADER\n$STRATA_BLOCK_CONTENT" >> "${AGENTS_MD}.tmp"
    
    # Clean up empty lines at the end of the file before overwriting
    awk 'NF > 0 {last = NR} {line[NR] = $0} END {for (i = 1; i <= last; i++) print line[i]}' "${AGENTS_MD}.tmp" > "$AGENTS_MD"
    rm "${AGENTS_MD}.tmp"
    echo "  -> OpenCode AGENTS.md updated successfully."
else
    echo "  -> Appending Strata block to $AGENTS_MD..."
    echo -e "\n$STRATA_BLOCK_HEADER\n$STRATA_BLOCK_CONTENT\n" >> "$AGENTS_MD"
    echo "  -> OpenCode AGENTS.md updated successfully."
fi
OPENCODE_CONFIG="$HOME/.config/opencode/opencode.json"

if [ -f "$OPENCODE_CONFIG" ]; then
    if command -v jq &>/dev/null; then
        # Safely update JSON using jq (standard bash tool)
        jq '.mcp |= (. // {}) | .mcp.strata = {"type": "local", "command": ["'"$HOME"'/.local/bin/strata-mcp"]}' "$OPENCODE_CONFIG" > "${OPENCODE_CONFIG}.tmp" && mv "${OPENCODE_CONFIG}.tmp" "$OPENCODE_CONFIG"
        echo "  -> OpenCode configuration updated successfully using jq."
    elif command -v node &>/dev/null; then
        # Fallback to Node.js (highly likely to be installed since AI agents use npx for tools like context7)
        node -e "
const fs = require('fs');
const path = require('path');
const filepath = process.argv[1];
let data = JSON.parse(fs.readFileSync(filepath, 'utf8'));
data.mcp = data.mcp || {};
data.mcp.strata = { type: 'local', command: [path.join(process.env.HOME, '.local/bin/strata-mcp')] };
fs.writeFileSync(filepath, JSON.stringify(data, null, 2));
" "$OPENCODE_CONFIG"
        echo "  -> OpenCode configuration updated successfully using Node.js fallback."
    else
        echo "  -> Notice: Neither 'jq' nor 'node' were found."
        echo "     Please manually add the following to your $OPENCODE_CONFIG under 'mcp':"
        echo "     \"strata\": { \"type\": \"local\", \"command\": [\"$HOME/.local/bin/strata-mcp\"] }"
    fi
else
    echo "  -> Warning: $OPENCODE_CONFIG not found."
fi

echo "Installation Complete!"
