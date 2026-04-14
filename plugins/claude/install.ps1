# Claude Code MCP Installer (PowerShell)
Write-Output "Setting up Claude Code MCP servers..."

$jsonContent = @'{
    "name": "claude-plugin",
    "mcpServers": {
        "server-name": {
            "command": "node",
            "args": ["${CLAUDE_PLUGIN_ROOT}/servers/server.js"],
            "env": {"API_KEY": "${API_KEY}"}
        }
    }
}'@

$jsonContent | Out-File -FilePath "plugin.json" -Encoding utf8
Write-Output "Configuration for Claude Code MCP saved."