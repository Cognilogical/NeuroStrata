$ErrorActionPreference = "Stop"

$scriptDir = $PSScriptRoot
$installPluginDir = Join-Path $env:USERPROFILE ".local\share\strata\plugin"
$opencodeConfig = Join-Path $env:USERPROFILE ".config\opencode\opencode.json"
$githubRepo = "your-username/strata"

Write-Host "Setting up the Strata OpenCode plugin..."
$pluginDir = Join-Path $scriptDir "strata-plugin"

$distDir = Join-Path $pluginDir "dist"
$pkgJson = Join-Path $pluginDir "package.json"

if ((Test-Path $distDir) -and (Test-Path $pkgJson)) {
    Write-Host "  -> Found local strata-plugin source. Copying to $installPluginDir..."
    if (-not (Test-Path $installPluginDir)) {
        New-Item -ItemType Directory -Force -Path $installPluginDir | Out-Null
    }
    Copy-Item -Path $distDir -Destination $installPluginDir -Recurse -Force
    Copy-Item -Path $pkgJson -Destination $installPluginDir -Force
} else {
    # Production Mode
    $pluginUrl = "https://github.com/$githubRepo/releases/latest/download/opencode-strata.tgz"
    Write-Host "  -> Downloading pre-compiled plugin from $pluginUrl..."
    
    if (-not (Test-Path $installPluginDir)) {
        New-Item -ItemType Directory -Force -Path $installPluginDir | Out-Null
    }
    $tgzPath = Join-Path $installPluginDir "plugin.tgz"
    try {
        Invoke-WebRequest -Uri $pluginUrl -OutFile $tgzPath
        # Use tar to extract (Windows 10+)
        # We need to extract the tarball to the directory.
        tar -xzf $tgzPath -C $installPluginDir --strip-components=1
        Remove-Item -Path $tgzPath -Force
        Write-Host "  -> Plugin downloaded and extracted successfully."
    } catch {
        Write-Error "Failed to download or extract pre-compiled plugin tarball."
        Write-Host "If you are compiling from source, please run .\mcp\build.ps1 first."
        exit 1
    }
}

# Patch OpenCode configuration
Write-Host "Patching OpenCode configuration..."
$configParent = Split-Path $opencodeConfig
if (-not (Test-Path $configParent)) {
    New-Item -ItemType Directory -Force -Path $configParent | Out-Null
}

if (-not (Test-Path $opencodeConfig)) {
    Set-Content -Path $opencodeConfig -Value "{}"
}

try {
    $configContent = Get-Content -Raw -Path $opencodeConfig
    $configData = $configContent | ConvertFrom-Json
    
    # Initialize objects if null
    if ($null -eq $configData) { $configData = @{} }
    if ($null -eq $configData.mcp) { Add-Member -InputObject $configData -MemberType NoteProperty -Name "mcp" -Value @{} }
    if ($null -eq $configData.plugin) { Add-Member -InputObject $configData -MemberType NoteProperty -Name "plugin" -Value @() }
    
    # Setup MCP strata config
    $strataMcpCommand = Join-Path $env:USERPROFILE ".local\bin\strata-mcp.exe"
    $mcpStrata = @{
        "type" = "local"
        "command" = @($strataMcpCommand)
    }
    $configData.mcp.strata = $mcpStrata
    
    # Setup plugin array
    $pluginArray = [System.Collections.ArrayList]@($configData.plugin)
    
    # Remove old global plugin if present
    if ($pluginArray.Contains("opencode-strata")) {
        $pluginArray.Remove("opencode-strata")
    }
    
    # Format the path replacing single backward slash with double
    $escapedInstallPluginDir = $installPluginDir.Replace("\", "\\")
    
    if (-not $pluginArray.Contains($escapedInstallPluginDir) -and -not $pluginArray.Contains($installPluginDir)) {
        $pluginArray.Add($escapedInstallPluginDir) | Out-Null
    }
    $configData.plugin = $pluginArray
    
    $jsonOut = $configData | ConvertTo-Json -Depth 10
    Set-Content -Path $opencodeConfig -Value $jsonOut
    Write-Host "  -> OpenCode configuration updated successfully."
} catch {
    Write-Host "  -> Notice: Could not parse or update JSON."
    Write-Host "     Please manually add the MCP configuration to $opencodeConfig."
}