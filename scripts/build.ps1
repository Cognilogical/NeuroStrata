#Requires -Version 5.1
<#
.SYNOPSIS
    Build neurostrata-mcp on Windows.

.DESCRIPTION
    Cargo cannot build this project on its own. lbug (LadybugDB) compiles a C++
    graph engine from vendored source, so the build needs cl.exe, CMake and
    Ninja -- and on an ordinary Windows PATH none of the three are present, even
    when Visual Studio is installed, because VS keeps them inside its own tree.

    This script installs nothing and does not replace cargo. It locates the
    toolchain, puts it on PATH for this process only, checks the three versions
    that are known to break the build, and hands off to cargo.

    The checks, and why each exists:

      * MSVC >= 14.40   14.37 (VS 2022 17.7) dies ~848 files into lbug's C++
                        build with "fatal error C1001: Internal compiler error"
                        in PackExpander.cpp. It is a front-end ICE, so lowering
                        optimisation does not help. This is stricter than "a
                        C++20 toolset": 14.37 is C++20 and still fails.

      * CMake >= 3.15   the highest cmake_minimum_required in lbug's tree.
                        CMake 4 is fine -- the one sub-3.5 declaration sits in
                        a re2 branch guarded by BUILD_SHARED_LIBS, which is off.

      * Ninja present   The cmake crate selects the Ninja generator here.
                        Without it, configuration fails with "CMake was unable
                        to find a build program corresponding to Ninja" -- a
                        message naming neither cargo nor NeuroStrata.

    Overrides: set NEUROSTRATA_CMAKE or NEUROSTRATA_NINJA to the full path of an
    executable to bypass discovery for that tool.

.PARAMETER Configuration
    release (default) or debug.

.PARAMETER CheckOnly
    Run every check, print the resolved toolchain, then stop without building.

.PARAMETER NoLocked
    Drop --locked. By default the build must match Cargo.lock exactly.

.PARAMETER VcToolsVersion
    Select a specific MSVC toolset, e.g. 14.44. Useful when several are
    installed and the newest is not the one you want to build with.

.EXAMPLE
    .\scripts\build.ps1 -CheckOnly
    Report the toolchain this machine would build with, and build nothing.

.EXAMPLE
    .\scripts\build.ps1
    Release build. Output lands at target\release\neurostrata-mcp.exe.

.EXAMPLE
    .\scripts\build.ps1 -Configuration debug --features something
    Arguments this script does not define are passed through to cargo.
#>
[CmdletBinding()]
param(
    [ValidateSet('release', 'debug')]
    [string]$Configuration = 'release',

    [switch]$CheckOnly,
    [switch]$NoLocked,
    [string]$VcToolsVersion,

    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CargoArgs
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$MinMsvc  = [version]'14.40'
$MinCMake = [version]'3.15'   # the highest cmake_minimum_required in lbug's tree

function Fail {
    param([string]$Message, [string[]]$Hint = @())
    Write-Host ''
    Write-Host "ERROR: $Message" -ForegroundColor Red
    foreach ($line in $Hint) { Write-Host "       $line" -ForegroundColor Yellow }
    exit 1
}

function Step { param([string]$Message) Write-Host "==> $Message" -ForegroundColor Cyan }

# Reads "cmake version 3.31.12" or "1.13.2" and returns a [version], or $null
# when the text carries no version at all.
function Get-VersionFromOutput {
    param([string]$Text)
    $match = [regex]::Match($Text, '(\d+)\.(\d+)(\.\d+)?')
    if (-not $match.Success) { return $null }
    try { return [version]$match.Value } catch { return $null }
}

function Add-ToPathFront {
    param([string]$Directory)
    $env:PATH = '{0};{1}' -f $Directory, $env:PATH
}

$repoRoot = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)

Write-Host ''
Write-Host 'neurostrata-mcp build (Windows)' -ForegroundColor White
Write-Host "repository: $repoRoot"

# ---------------------------------------------------------------------------
# 1. cargo
#
# Rust is commonly installed but off PATH, so a bare Get-Command lookup failing
# is not evidence that it is absent. Check the standard homes too.
# ---------------------------------------------------------------------------
Step 'Locating cargo'

$cargo = $null
$onPath = Get-Command cargo.exe -ErrorAction SilentlyContinue
if ($onPath) { $cargo = $onPath.Source }

if (-not $cargo) {
    $cargoHomes = @()
    if ($env:CARGO_HOME) { $cargoHomes += $env:CARGO_HOME }
    if ($env:USERPROFILE) { $cargoHomes += (Join-Path $env:USERPROFILE '.cargo') }
    foreach ($cargoHome in $cargoHomes) {
        $candidate = Join-Path $cargoHome 'bin\cargo.exe'
        if (Test-Path $candidate) { $cargo = $candidate; break }
    }
}

if (-not $cargo) {
    Fail 'cargo was not found.' @(
        'Install the Rust toolchain from https://rustup.rs, host x86_64-pc-windows-msvc.',
        'If Rust is already installed it is simply off PATH: set CARGO_HOME, or add',
        '%USERPROFILE%\.cargo\bin to PATH, and run this again.'
    )
}
Add-ToPathFront (Split-Path -Parent $cargo)
$cargoVersion = (& $cargo --version) -join ' '

# ---------------------------------------------------------------------------
# 2. MSVC, via vswhere and vcvars64
#
# vcvars64.bat is used rather than hand-assembled PATH entries because cl.exe
# also needs INCLUDE, LIB and LIBPATH, and those are not guessable. The batch
# file runs in a child cmd.exe and the environment it produces is imported here.
# ---------------------------------------------------------------------------
Step 'Locating the MSVC toolchain'

$vswhere = $null
$vswhereCandidates = @()
if (${env:ProgramFiles(x86)}) {
    $vswhereCandidates += (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe')
}
if ($env:ProgramFiles) {
    $vswhereCandidates += (Join-Path $env:ProgramFiles 'Microsoft Visual Studio\Installer\vswhere.exe')
}
foreach ($candidate in $vswhereCandidates) {
    if (Test-Path $candidate) { $vswhere = $candidate; break }
}

if (-not $vswhere) {
    Fail 'vswhere.exe was not found, so no Visual Studio installation could be located.' @(
        'Install Visual Studio (Community is enough) with the "Desktop development',
        'with C++" workload, or the standalone Build Tools:',
        'https://visualstudio.microsoft.com/downloads/'
    )
}

$vsPath = & $vswhere -latest -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property installationPath
if ($vsPath) { $vsPath = ($vsPath | Select-Object -First 1).Trim() }

if (-not $vsPath) {
    Fail 'No Visual Studio installation carries the MSVC x64 C++ tools.' @(
        'Visual Studio can be installed without them. In the Visual Studio Installer,',
        'add the "Desktop development with C++" workload.'
    )
}

$vcvars = Join-Path $vsPath 'VC\Auxiliary\Build\vcvars64.bat'
if (-not (Test-Path $vcvars)) {
    Fail "Found Visual Studio at $vsPath but no vcvars64.bat under VC\Auxiliary\Build." @(
        'The C++ build tools component looks incomplete; repair it in the Visual Studio Installer.'
    )
}

# vcvars64.bat is chatty on stderr even when it succeeds -- it probes for tools
# it can do without. Park that output in a file and surface it only if the batch
# file actually fails, so a working build stays quiet without hiding a real error.
$vcvarsLog = [System.IO.Path]::GetTempFileName()
$vcvarsCommand = 'call "{0}"' -f $vcvars
if ($VcToolsVersion) { $vcvarsCommand += ' -vcvars_ver={0}' -f $VcToolsVersion }
$vcvarsCommand += ' > nul 2> "{0}" && set' -f $vcvarsLog

$importedEnvironment = & $env:ComSpec /s /c "$vcvarsCommand"
$vcvarsExitCode = $LASTEXITCODE

if ($vcvarsExitCode -ne 0) {
    $hints = @("Visual Studio: $vsPath")
    if ($VcToolsVersion) {
        $hints += "-VcToolsVersion $VcToolsVersion may name a toolset that is not installed."
        $hints += "The installed toolsets are the directory names under $vsPath\VC\Tools\MSVC."
    }
    $vcvarsOutput = @(Get-Content -Path $vcvarsLog -ErrorAction SilentlyContinue)
    if ($vcvarsOutput.Count -gt 0) {
        $hints += ''
        $hints += 'vcvars64.bat said:'
        foreach ($line in $vcvarsOutput) { $hints += "  $line" }
    }
    Remove-Item -Path $vcvarsLog -ErrorAction SilentlyContinue
    Fail ('vcvars64.bat failed (exit {0}).' -f $vcvarsExitCode) $hints
}
Remove-Item -Path $vcvarsLog -ErrorAction SilentlyContinue

foreach ($line in $importedEnvironment) {
    $split = $line.IndexOf('=')
    if ($split -lt 1) { continue }
    [Environment]::SetEnvironmentVariable($line.Substring(0, $split), $line.Substring($split + 1), 'Process')
}

if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
    Fail 'vcvars64.bat ran but cl.exe is still not on PATH.' @("Visual Studio: $vsPath")
}

$msvcVersion = $null
if ($env:VCToolsVersion) { $msvcVersion = Get-VersionFromOutput $env:VCToolsVersion }
if (-not $msvcVersion) {
    Fail 'Could not determine the MSVC toolset version: vcvars64.bat did not set VCToolsVersion.'
}

# Compare major.minor only. 14.40 against 14.37 is the distinction that matters;
# the build number carries no ordering information for this check.
$msvcMajorMinor = [version]('{0}.{1}' -f $msvcVersion.Major, $msvcVersion.Minor)
if ($msvcMajorMinor -lt $MinMsvc) {
    Fail ('MSVC toolset {0} is older than {1}.' -f $env:VCToolsVersion, $MinMsvc) @(
        'MSVC 14.37 fails partway through lbug''s C++ build with a front-end internal',
        'compiler error (C1001, PackExpander.cpp). Being C++20-capable is not enough.',
        'Install a newer C++ toolset in the Visual Studio Installer, or pass',
        '-VcToolsVersion to select one already present. The installed toolsets are the',
        "directory names under $vsPath\VC\Tools\MSVC."
    )
}

# ---------------------------------------------------------------------------
# 3. CMake
#
# Any CMake >= 3.15 works, including the 4.x that VS 2026 bundles. vcvars64.bat
# has already put the VS copy on PATH by this point, so that is usually what
# wins; the other candidates cover machines whose VS has no CMake component.
# NEUROSTRATA_CMAKE overrides all of it.
# ---------------------------------------------------------------------------
Step 'Locating CMake'

# An override is authoritative: if NEUROSTRATA_CMAKE names something unusable,
# say so rather than quietly building with a different cmake than was asked for.
$cmakeOverridden = [bool]$env:NEUROSTRATA_CMAKE
$cmakeCandidates = @()

if ($cmakeOverridden) {
    $cmakeCandidates = @($env:NEUROSTRATA_CMAKE)
    if (-not (Test-Path $env:NEUROSTRATA_CMAKE)) {
        Fail ('NEUROSTRATA_CMAKE points at a path that does not exist: {0}' -f $env:NEUROSTRATA_CMAKE)
    }
} else {
    $onPath = Get-Command cmake.exe -ErrorAction SilentlyContinue
    if ($onPath) { $cmakeCandidates += $onPath.Source }
    if ($env:USERPROFILE) {
        $localTools = Join-Path $env:USERPROFILE '.local\tools'
        if (Test-Path $localTools) {
            # Newest extracted copy first, so a 3.31 beside a 3.20 wins.
            $extracted = Get-ChildItem -Path $localTools -Directory -Filter 'cmake-*' -ErrorAction SilentlyContinue |
                Sort-Object Name -Descending
            foreach ($directory in $extracted) {
                $candidate = Join-Path $directory.FullName 'bin\cmake.exe'
                if (Test-Path $candidate) { $cmakeCandidates += $candidate }
            }
        }
    }
    $cmakeCandidates += (Join-Path $vsPath 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe')
}

$cmake = $null
$cmakeVersion = $null
$rejectedCMakes = @()

foreach ($candidate in ($cmakeCandidates | Select-Object -Unique)) {
    if (-not (Test-Path $candidate)) { continue }
    $reported = $null
    try { $reported = (& $candidate --version 2>&1 | Select-Object -First 1) } catch { continue }
    $version = Get-VersionFromOutput ([string]$reported)
    if (-not $version) { continue }

    if ($version -lt $MinCMake) {
        $rejectedCMakes += ('{0}  ({1}, older than {2})' -f $candidate, $version, $MinCMake)
        continue
    }
    $cmake = $candidate
    $cmakeVersion = $version
    break
}

if (-not $cmake) {
    $hints = @()
    if ($rejectedCMakes.Count -gt 0) {
        $hints += 'Rejected:'
        foreach ($rejected in $rejectedCMakes) { $hints += "  $rejected" }
        $hints += ''
    }
    $hints += 'Install CMake 3.15 or newer -- any 4.x is fine too -- from'
    $hints += 'https://cmake.org/download/. Extracting the zip is enough: no'
    $hints += 'administrator rights and no PATH change are needed if it goes under'
    $hints += '%USERPROFILE%\.local\tools\, which this script searches.'
    $hints += ''
    $hints += 'Alternatively point NEUROSTRATA_CMAKE at a cmake.exe.'
    if ($cmakeOverridden) {
        Fail 'NEUROSTRATA_CMAKE names a CMake this build cannot use.' $hints
    }
    Fail 'No usable CMake was found.' $hints
}

Add-ToPathFront (Split-Path -Parent $cmake)

# ---------------------------------------------------------------------------
# 4. Ninja
#
# Any version does. The failure this prevents is absence, not staleness.
# ---------------------------------------------------------------------------
Step 'Locating Ninja'

$ninjaOverridden = [bool]$env:NEUROSTRATA_NINJA
$ninjaCandidates = @()

if ($ninjaOverridden) {
    $ninjaCandidates = @($env:NEUROSTRATA_NINJA)
    if (-not (Test-Path $env:NEUROSTRATA_NINJA)) {
        Fail ('NEUROSTRATA_NINJA points at a path that does not exist: {0}' -f $env:NEUROSTRATA_NINJA)
    }
} else {
    $onPath = Get-Command ninja.exe -ErrorAction SilentlyContinue
    if ($onPath) { $ninjaCandidates += $onPath.Source }
    if ($env:USERPROFILE) {
        $ninjaCandidates += (Join-Path $env:USERPROFILE '.local\tools\ninja\ninja.exe')
    }
    $ninjaCandidates += (Join-Path $vsPath 'Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja\ninja.exe')
}

$ninja = $null
$ninjaVersion = $null
foreach ($candidate in ($ninjaCandidates | Select-Object -Unique)) {
    if (-not (Test-Path $candidate)) { continue }
    $reported = $null
    try { $reported = (& $candidate --version 2>&1 | Select-Object -First 1) } catch { continue }
    $version = Get-VersionFromOutput ([string]$reported)
    if (-not $version) { continue }
    $ninja = $candidate
    $ninjaVersion = $version
    break
}

if (-not $ninja) {
    Fail 'Ninja was not found.' @(
        'The cmake crate selects the Ninja generator for this build. Without ninja the',
        'failure reads "CMake was unable to find a build program corresponding to Ninja",',
        'which names neither cargo nor NeuroStrata.',
        '',
        'Visual Studio normally bundles one at',
        'Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja\ninja.exe. If yours does not,',
        'take a release from https://github.com/ninja-build/ninja/releases and drop',
        'ninja.exe into %USERPROFILE%\.local\tools\ninja\, which this script searches.'
    )
}
Add-ToPathFront (Split-Path -Parent $ninja)

# ---------------------------------------------------------------------------
# 5. Report
# ---------------------------------------------------------------------------
Write-Host ''
Write-Host 'Toolchain' -ForegroundColor White
Write-Host ('  cargo   {0}' -f $cargoVersion)
Write-Host ('          {0}' -f $cargo) -ForegroundColor DarkGray
Write-Host ('  msvc    {0}' -f $env:VCToolsVersion)
Write-Host ('          {0}' -f $vsPath) -ForegroundColor DarkGray
Write-Host ('  cmake   {0}' -f $cmakeVersion)
Write-Host ('          {0}' -f $cmake) -ForegroundColor DarkGray
Write-Host ('  ninja   {0}' -f $ninjaVersion)
Write-Host ('          {0}' -f $ninja) -ForegroundColor DarkGray
Write-Host ''

if ($CheckOnly) {
    Write-Host 'Checks passed. -CheckOnly was set, so nothing was built.' -ForegroundColor Green
    exit 0
}

# ---------------------------------------------------------------------------
# 6. Build
# ---------------------------------------------------------------------------
$cargoArgList = @('build')
if ($Configuration -eq 'release') {
    $cargoArgList += '--release'
} else {
    # Worth 20 seconds of warning rather than 25 minutes of compiling: a debug
    # build gives the vendored C++ full debug info, and the static lbug.lib then
    # passes the 4 GiB ceiling for a COFF image -- LNK1248, at the very last link
    # step. Nothing about the toolchain checks above predicts it.
    Write-Host '    NOTE: debug builds are not known to link on Windows. The vendored C++' -ForegroundColor Yellow
    Write-Host '          static library passes the 4 GiB COFF limit and the link fails with' -ForegroundColor Yellow
    Write-Host '          LNK1248 after a full compile. Use -Configuration release.' -ForegroundColor Yellow
}
if (-not $NoLocked) { $cargoArgList += '--locked' }
if ($CargoArgs) { $cargoArgList += ($CargoArgs | Where-Object { $_ -ne '--' }) }

Step ('cargo ' + ($cargoArgList -join ' '))
Write-Host '    A cold build compiles lbug''s C++ engine from source. Expect several minutes.' -ForegroundColor DarkGray
Write-Host ''

Push-Location $repoRoot
try {
    & $cargo @cargoArgList
    $exitCode = $LASTEXITCODE
} finally {
    Pop-Location
}

if ($exitCode -ne 0) {
    Write-Host ''
    Write-Host "cargo exited $exitCode." -ForegroundColor Red
    Write-Host 'If a single .obj failed with "C1056: cannot update the time date stamp field",' -ForegroundColor Yellow
    Write-Host 'that is a file lock -- characteristic of real-time antivirus, not a toolchain' -ForegroundColor Yellow
    Write-Host 'defect. Ninja resumes cleanly on a re-run.' -ForegroundColor Yellow
    exit $exitCode
}

$binary = Join-Path $repoRoot ('target\{0}\neurostrata-mcp.exe' -f $Configuration)
Write-Host ''
if (Test-Path $binary) {
    $sizeMb = [math]::Round((Get-Item $binary).Length / 1MB, 1)
    Write-Host ('Built {0} ({1} MB)' -f $binary, $sizeMb) -ForegroundColor Green
} else {
    Write-Host 'cargo reported success but the expected binary is not at' -ForegroundColor Yellow
    Write-Host "  $binary" -ForegroundColor Yellow
}
exit 0
