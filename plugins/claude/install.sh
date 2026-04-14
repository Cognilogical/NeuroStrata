#!/bin/bash

# Claude Code MCP Server Installer
echo "Setting up Claude Code MCP servers..."

echo "Generating plugin.json and .mcp.json dynamically..."
cat <<EOF > plugin.json
{
  "name": "claude-plugin",
  "mcpServers": {
    "server-name": {
      "command": "node",
      "args": ["${CLAUDE_PLUGIN_ROOT}/servers/server.js"],
      "env": {"API_KEY": "${API_KEY}"}
    }
  }
}
EOF
