#!/bin/bash
set -e

# ==============================================================================
# Configuration
# Change this to your actual GitHub username and repository when publishing
GITHUB_REPO="your-username/neurostrata"
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
    BINARY_NAME="neurostrata-mcp-windows-amd64.exe"
else
    BINARY_NAME="neurostrata-mcp-${OS}-${ARCH}"
fi

INSTALL_DIR="$HOME/.local/share/neurostrata/bin"
mkdir -p "$INSTALL_DIR"
DEST_BIN="$INSTALL_DIR/neurostrata-mcp"

echo "Installing NeuroStrata Go MCP Server ($BINARY_NAME)..."

# 1. Detect if we are installing from source (development mode)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCAL_BIN="$SCRIPT_DIR/../bin/$BINARY_NAME"
LOCAL_SKILL_DIR="$SCRIPT_DIR/../.agents/skills/neurostrata"

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
ln -sf "$DEST_BIN" "$HOME/.local/bin/neurostrata-mcp"
echo "  -> Linked $DEST_BIN to ~/.local/bin/neurostrata-mcp"

# Symlink skill to ~/.agents/skills/neurostrata
mkdir -p "$HOME/.agents/skills/neurostrata"
if [ -d "$LOCAL_SKILL_DIR" ]; then
    # We are in the source tree, symlink the local directory
    ln -sfn "$LOCAL_SKILL_DIR" "$HOME/.agents/skills/neurostrata"
    echo "  -> Linked local $LOCAL_SKILL_DIR to ~/.agents/skills/neurostrata"
else
    # We are downloading raw, fetch the SKILL.md from GitHub
    curl -f -s -o "$HOME/.agents/skills/neurostrata/SKILL.md" "https://raw.githubusercontent.com/$GITHUB_REPO/main/.agents/skills/neurostrata/SKILL.md"
    echo "  -> Downloaded SKILL.md directly from GitHub to ~/.agents/skills/neurostrata"
fi

# Symlink agent
mkdir -p "$HOME/.config/opencode/agents"
if [ -d "$SCRIPT_DIR/../.agents/agents" ]; then
    ln -sfn "$SCRIPT_DIR/../.agents/agents/neurostrata-task-agent.md" "$HOME/.config/opencode/agents/neurostrata-task-agent.md"
    echo "  -> Linked neurostrata-task-agent.md to ~/.config/opencode/agents/"
fi
CONFIG_DIR="$HOME/.config/neurostrata"
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
  "qdrant_collection": "neurostrata",
  "http_port": "8005"
}
JSON
  echo "Default configuration created."
else
  echo "Configuration already exists."
fi

echo "MCP Server Installation Complete!"
