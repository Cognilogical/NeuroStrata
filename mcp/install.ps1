$ErrorActionPreference = "Stop"

# ==============================================================================
# Configuration
$githubRepo = "your-username/strata"
# ==============================================================================

$os = "windows"
$arch = "amd64" # Simplified for typical Windows installations

$binaryName = "strata-mcp-${os}-${arch}.exe"

$installDir = Join-Path $env:USERPROFILE ".local\share\strata\bin"
if (-not (Test-Path $installDir)) {
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
}

$destBin = Join-Path $installDir "strata-mcp.exe"

Write-Host "Installing Strata Go MCP Server ($binaryName)..."

# 1. Detect if we are installing from source
$scriptDir = $PSScriptRoot
$localBin = Join-Path $scriptDir "..\bin\strata-mcp.exe"
$localSkillDir = Join-Path $scriptDir "..\.agents\skills\strata"

if (Test-Path $localBin) {
    Write-Host "Found local compiled binary, copying to $destBin..."
    Copy-Item -Path $localBin -Destination $destBin -Force
} else {
    # 2. Production Mode
    $downloadUrl = "https://github.com/$githubRepo/releases/latest/download/$binaryName"
    Write-Host "Downloading pre-compiled binary from $downloadUrl..."
    
    try {
        Invoke-WebRequest -Uri $downloadUrl -OutFile $destBin
        Write-Host "Download successful."
    } catch {
        Write-Error "Failed to download pre-compiled binary ($binaryName)."
        Write-Host "If you are compiling from source, please run .\build.ps1 first before running .\install.ps1."
        exit 1
    }
}

# Set up local bin for PATH
$localBinDir = Join-Path $env:USERPROFILE ".local\bin"
if (-not (Test-Path $localBinDir)) {
    New-Item -ItemType Directory -Force -Path $localBinDir | Out-Null
}

$symlinkBin = Join-Path $localBinDir "strata-mcp.exe"
Write-Host "Setting up executable link at $symlinkBin..."
Copy-Item -Path $destBin -Destination $symlinkBin -Force
Write-Host "  -> Copied $destBin to $symlinkBin"

# Set up SKILL.md
$skillDir = Join-Path $env:USERPROFILE ".agents\skills\strata"
if (-not (Test-Path $skillDir)) {
    New-Item -ItemType Directory -Force -Path $skillDir | Out-Null
}

if (Test-Path $localSkillDir) {
    Write-Host "  -> Copying local $localSkillDir to $skillDir"
    Copy-Item -Path "$localSkillDir\*" -Destination $skillDir -Recurse -Force
} else {
    Write-Host "  -> Downloaded SKILL.md directly from GitHub to $skillDir"
    Invoke-WebRequest -Uri "https://raw.githubusercontent.com/$githubRepo/main/.agents/skills/strata/SKILL.md" -OutFile "$skillDir\SKILL.md"
}

# Symlink/copy agent
$opencodeAgentsDir = Join-Path $env:USERPROFILE ".config\opencode\agents"
if (-not (Test-Path $opencodeAgentsDir)) {
    New-Item -ItemType Directory -Force -Path $opencodeAgentsDir | Out-Null
}

$localAgent = Join-Path $scriptDir "..\.agents\agents\strata-task-agent.md"
if (Test-Path $localAgent) {
    $destAgent = Join-Path $opencodeAgentsDir "strata-task-agent.md"
    Copy-Item -Path $localAgent -Destination $destAgent -Force
    Write-Host "  -> Copied strata-task-agent.md to $opencodeAgentsDir\"
}

# Config
$configDir = Join-Path $env:USERPROFILE ".config\strata"
$configFile = Join-Path $configDir "config.json"
Write-Host "Setting up configuration at $configFile..."

if (-not (Test-Path $configDir)) {
    New-Item -ItemType Directory -Force -Path $configDir | Out-Null
}

if (-not (Test-Path $configFile)) {
    $configContent = @"
{
  "embedder_url": "http://localhost:8004/v1/embeddings",
  "embedder_model": "nomic-embed-text-v1.5.f16.gguf",
  "embedder_api_key": "sk-local",
  "qdrant_url": "http://localhost:6333",
  "qdrant_collection": "strata",
  "http_port": "8005"
}
"@
    Set-Content -Path $configFile -Value $configContent
    Write-Host "Default configuration created."
} else {
    Write-Host "Configuration already exists."
}

Write-Host "MCP Server Installation Complete!"