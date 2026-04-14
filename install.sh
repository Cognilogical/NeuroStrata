#!/usr/bin/env bash
set -e
echo "==============================================="
echo "  Strata: 3-Tier Memory Installer        "
echo "==============================================="



# 1. Check for Podman
if ! command -v podman &> /dev/null; then
    echo "Error: Podman is not installed. Please install Podman first: https://podman.io/docs/installation"
    exit 1
fi

if ! command -v podman-compose &> /dev/null; then
    echo "Installing podman-compose via pip..."
    pip3 install podman-compose --user
fi


# 3. Install graphify (Codebase Spatial Mapping)
echo "Installing graphify..."
pip install pipx && pipx install graphify-cli || echo "Note: graphify-cli package may need specific repo access."

# 4. Install beads (Issue Tracker)
echo "Installing beads issue tracker..."
if command -v go &> /dev/null; then
    # Assuming beads is a Go tool. If it's distributed differently, update this.
    go install github.com/beads/bd@latest || echo "Warning: Could not install beads via go install. Ensure bd is in your PATH."
else
    echo "Warning: Go is not installed. Skipping beads compilation. Please install beads manually."
fi

# 5. Pull Ollama models (Embedder & Lightweight LLM)
echo "Starting local infrastructure to pull models..."
podman rm -f strata-qdrant strata-embedder || true && podman-compose up -d

echo "Waiting for Ollama to start..."
sleep 10

echo "Pulling local embedding model (nomic-embed-text)..."
podman exec -it ollama ollama pull nomic-embed-text

echo "Pulling lightweight local LLM (llama3.2:1b)..."
podman exec -it ollama ollama pull llama3.2:1b

# 6. Install Go MCP Server
echo "Installing Core MCP Server..."
if [ -f "./mcp/install.sh" ]; then
    bash ./mcp/install.sh
else
    echo "Warning: ./mcp/install.sh not found."
fi

# 7. Client Discovery and Plugin Setup
echo "Detecting available client environments..."
if command -v opencode &> /dev/null || [ -d "$HOME/.config/opencode" ]; then
    echo "OpenCode detected. Running OpenCode plugin setup..."
    if [ -f "./plugins/opencode/install.sh" ]; then
        bash ./plugins/opencode/install.sh
    fi
fi

if command -v openclaw &> /dev/null || [ -d "$HOME/.config/openclaw" ]; then
    echo "OpenClaw detected. Running OpenClaw plugin setup..."
    if [ -f "./plugins/openclaw/install.sh" ]; then
        bash ./plugins/openclaw/install.sh
    fi
fi

if command -v claude &> /dev/null || [ -d "$HOME/.claude" ]; then
    echo "Claude Code detected. Running Claude plugin setup..."
    if [ -f "./plugins/claude/install.sh" ]; then
        bash ./plugins/claude/install.sh
    fi
fi

echo "==============================================="
echo "  Installation Complete!                       "
echo "==============================================="
