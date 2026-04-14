$ErrorActionPreference = "Stop"

$scriptDir = $PSScriptRoot
$installPluginDir = Join-Path $env:USERPROFILE ".local\share\strata\plugin"
$openClawConfig = Join-Path $env:USERPROFILE ".config\openclaw\openclaw.json"
$githubRepo = "your-username/strata"

Write-Host "Setting up the Strata OpenClaw plugin..."
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
    $pluginUrl = "https://github.com/$githubRepo/releases/latest/download/openclaw-strata.tgz"
    Write-Host "  -> Downloading pre-compiled plugin from $pluginUrl..."
    if (-not (Test-Path $installPluginDir)) {
        New-Item -ItemType Directory -Force -Path $installPluginDir | Out-Null
    }
    $tgzPath = Join-Path $installPluginDir "plugin.tgz"
    try {
        Invoke-WebRequest -Uri $pluginUrl -OutFile $tgzPath
        tar -xzf $tgzPath -C $installPluginDir --strip-components=1
        Remove-Item -Path $tgzPath -Force
        Write-Host "  -> Plugin downloaded and extracted successfully."
    } catch {
        Write-Error "Failed to download or extract pre-compiled plugin tarball."
        Write-Host "If you are compiling from source, please run .\mcp\build.ps1 first."
        exit 1
    }
}

Write-Host "Patching OpenClaw configuration..."
$configParent = Split-Path $openClawConfig
if (-not (Test-Path $configParent)) {
    New-Item -ItemType Directory -Force -Path $configParent | Out-Null
}

if (-not (Test-Path $openClawConfig)) {
    Set-Content -Path $openClawConfig -Value "{}"
}

try {
    $configContent = Get-Content -Raw -Path $openClawConfig
    $configData = $configContent | ConvertFrom-Json
    if ($null -eq $configData) { $configData = @{} }
    if ($null -eq $configData.plugin) { Add-Member -InputObject $configData -MemberType NoteProperty -Name "plugin" -Value @() }
    $pluginArray = [System.Collections.ArrayList]@($configData.plugin)
    if ($pluginArray.Contains("openclaw-strata")) {
        $pluginArray.Remove("openclaw-strata")
    }
    $pluginArray.Add($installPluginDir) | Out-Null
    $configData.plugin = $pluginArray
    $jsonOut = $configData | ConvertTo-Json -Depth 10
    Set-Content -Path $openClawConfig -Value $jsonOut
    Write-Host "  -> OpenClaw configuration updated successfully."
} catch {
    Write-Host "  -> Notice: Could not parse or update JSON."
    Write-Host "     Please manually add the plugin path to $openClawConfig."
}