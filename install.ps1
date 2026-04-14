$ErrorActionPreference = "Stop"

Write-Host "==============================================="
Write-Host "  Strata: 3-Tier Memory Installer (Windows)  "
Write-Host "==============================================="

# 1. Check for Podman
if (-not (Get-Command podman -ErrorAction SilentlyContinue)) {
    Write-Error "Podman is not installed. Please install Podman first: https://podman.io/docs/installation"
    exit 1
}

if (-not (Get-Command podman-compose -ErrorAction SilentlyContinue)) {
    Write-Host "Installing podman-compose via pip..."
    pip3 install podman-compose --user
}

# 3. Install graphify
Write-Host "Installing graphify..."
if (Get-Command pip -ErrorAction SilentlyContinue) {
    pip install pipx
    pipx install graphify-cli
} else {
    Write-Host "Note: pip is not found, skipping graphify installation."
}

# 4. Install beads
Write-Host "Installing beads issue tracker..."
if (Get-Command go -ErrorAction SilentlyContinue) {
    go install github.com/beads/bd@latest
} else {
    Write-Host "Warning: Go is not installed. Skipping beads compilation. Please install beads manually."
}

# 5. Pull Ollama models
Write-Host "Starting local infrastructure to pull models..."
podman rm -f strata-qdrant strata-embedder 2>$null
podman-compose up -d

Write-Host "Waiting for Ollama to start..."
Start-Sleep -Seconds 10

Write-Host "Pulling local embedding model (nomic-embed-text)..."
podman exec -it ollama ollama pull nomic-embed-text

Write-Host "Pulling lightweight local LLM (llama3.2:1b)..."
podman exec -it ollama ollama pull llama3.2:1b

# 6. Install Go MCP Server
Write-Host "Installing Core MCP Server..."
$mcpInstallScript = Join-Path $PSScriptRoot "mcp\install.ps1"
if (Test-Path $mcpInstallScript) {
    & $mcpInstallScript
} else {
    Write-Host "Warning: $mcpInstallScript not found."
}

# 7. Client Discovery and Plugin Setup
Write-Host "Detecting available client environments..."
$opencodeConfigDir = Join-Path $env:USERPROFILE ".config\opencode"
if ((Get-Command opencode -ErrorAction SilentlyContinue) -or (Test-Path $opencodeConfigDir)) {
    Write-Host "OpenCode detected. Running OpenCode plugin setup..."
    $pluginInstallScript = Join-Path $PSScriptRoot "plugins\opencode\install.ps1"
    if (Test-Path $pluginInstallScript) {
        & $pluginInstallScript
    }
}

$claudeConfigDir = Join-Path $env:USERPROFILE ".claude"
if ((Get-Command claude -ErrorAction SilentlyContinue) -or (Test-Path $claudeConfigDir)) {
    Write-Host "Claude Code detected. No specific setup needed currently."
}

Write-Host "==============================================="
Write-Host "  Installation Complete!                       "
Write-Host "==============================================="