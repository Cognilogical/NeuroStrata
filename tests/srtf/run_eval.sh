#!/usr/bin/env bash
set -e

# Navigate to the SRTF tests directory
cd "$(dirname "$0")"
SRTF_DIR="$(pwd)"
PROJECT_ROOT="$(cd ../.. && pwd)"
HOST_BINARY="$PROJECT_ROOT/target/release/neurostrata-mcp"

# Ensure the host binary actually exists
if [ ! -f "$HOST_BINARY" ]; then
    echo "❌ Error: neurostrata-mcp binary not found at $HOST_BINARY"
    echo "💡 Please run 'cargo build --release' in the project root first."
    exit 1
fi

# Ensure workspace and configs exist
mkdir -p "$SRTF_DIR/workspace"
mkdir -p "$SRTF_DIR/configs"

echo "==============================================="
echo "  NeuroStrata Self-Reinforced Testing Framework  "
echo "==============================================="
echo "🔧 Rebuilding/Verifying Base Image..."
podman build -t neurostrata-srtf-base -f Containerfile .

echo "🚀 Launching Isolated Sandbox Trial..."

# Start the container with read-only bind mounts for the binary and configs!
# Any changes to target/release/neurostrata-mcp or tests/srtf/configs/ on the host
# will be instantly available in the next run without a container rebuild!
podman run --rm -it \
  --userns=keep-id \
  -v "$HOST_BINARY:/usr/local/bin/neurostrata-mcp:ro" \
  -v "$SRTF_DIR/configs/AGENTS.md:/home/agent/workspace/AGENTS.md:ro" \
  -v "$SRTF_DIR/configs/SKILL.md:/home/agent/.agents/skills/neurostrata/SKILL.md:ro" \
  -v "$SRTF_DIR/configs/opencode.json:/home/agent/workspace/.config/opencode/opencode.json:ro" \
  -v "$SRTF_DIR/workspace:/home/agent/workspace:rw" \
  neurostrata-srtf-base \
  bash -c "
    if [ ! -d \"express\" ]; then
      echo \"📦 Cloning testbed repository (Express.js)...\"
      git clone --depth 1 https://github.com/expressjs/express.git
    fi
    cd express
    echo \"🧪 Initiating Subject Agent Trial...\"
    opencode --task \"Refactor routing and document the architecture.\"
  "
