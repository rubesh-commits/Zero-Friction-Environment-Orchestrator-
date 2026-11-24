#requires -Version 5.1
<#
.SYNOPSIS
    Launches the fresh-machine acceptance suite inside Windows Sandbox.

.DESCRIPTION
    Windows Sandbox starts from a clean Windows image with no Python, Node.js,
    Java, Git or build tools. This script builds the release binary, generates a
    temporary .wsb configuration that maps only the binary, the fixtures and the
    test scripts (all read-only), and starts the sandbox.

    The sandbox needs network access the first time so the orchestrator can
    fetch a minimal Python runtime and verify its SHA-256 checksum.

    Requires Windows 10 Pro/Enterprise or Windows 11 with the optional
    "Windows Sandbox" feature enabled.

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File .\enter-sandbox.ps1

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File .\enter-sandbox.ps1 -Wait
#>
[CmdletBinding()]
param(
    [switch]$Wait,
    [switch]$NoBuild,
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$WorkspaceRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$ReleaseDir = Join-Path $WorkspaceRoot 'target\release'
$Binary = Join-Path $ReleaseDir 'winorchestrator.exe'
$Fixtures = Join-Path $WorkspaceRoot 'crates\cli\tests\fixtures'

if (-not $NoBuild -and -not (Test-Path $Binary)) {
    Write-Host 'Building the release binary...'
    Push-Location $WorkspaceRoot
    try {
        & cargo build --release
        if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
    }
    finally { Pop-Location }
}
if (-not (Test-Path $Binary)) {
    throw "Release binary not found at $Binary. Run cargo build --release first."
}

$sandboxExe = Join-Path $env:SystemRoot 'System32\WindowsSandbox.exe'

function Escape-Xml([string]$Text) {
    return $Text.Replace('&', '&amp;').Replace('<', '&lt;').Replace('>', '&gt;').Replace('"', '&quot;')
}

$hostRelease = Escape-Xml $ReleaseDir
$hostFixtures = Escape-Xml $Fixtures
$hostTests = Escape-Xml $PSScriptRoot
$wsbPath = Join-Path ([System.IO.Path]::GetTempPath()) ("winorchestrator-fresh-{0}.wsb" -f [System.Diagnostics.Process]::GetCurrentProcess().Id)

$wsb = @"
<Configuration>
  <Networking>Default</Networking>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>$hostRelease</HostFolder>
      <SandboxFolder>C:\wo-bin</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>$hostFixtures</HostFolder>
      <SandboxFolder>C:\wo-fixtures</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>$hostTests</HostFolder>
      <SandboxFolder>C:\wo-tests</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
  </MappedFolders>
  <LogonCommand>
    <Command>powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:\wo-tests\inside-sandbox.ps1 -Binary C:\wo-bin\winorchestrator.exe -Fixtures C:\wo-fixtures</Command>
  </LogonCommand>
</Configuration>
"@

[System.IO.File]::WriteAllText($wsbPath, $wsb, (New-Object System.Text.UTF8Encoding($false)))
Write-Host "Sandbox configuration: $wsbPath"
if ($DryRun) {
    [xml]$parsed = $wsb
    if ($parsed.Configuration.MappedFolders.MappedFolder.Count -ne 3) {
        throw 'generated .wsb does not contain the three read-only mapped folders'
    }
    Write-Host $wsb
    Write-Host 'Dry run complete; Windows Sandbox was not started.'
    exit 0
}
if (-not (Test-Path $sandboxExe)) {
    $command = Get-Command WindowsSandbox.exe -ErrorAction SilentlyContinue
    if ($null -eq $command) {
        throw @'
Windows Sandbox is not available on this machine.
Enable it with:
  Enable-WindowsOptionalFeature -Online -FeatureName Containers-DisposableClientVM -All
Then reboot and run this script again.
'@
    }
    $sandboxExe = $command.Source
}
Write-Host 'Starting Windows Sandbox. Results appear on the sandbox desktop as winorchestrator-sandbox-*.log.'

$process = if ($Wait) {
    Start-Process -FilePath $sandboxExe -ArgumentList "`"$wsbPath`"" -Wait -PassThru
} else {
    Start-Process -FilePath $sandboxExe -ArgumentList "`"$wsbPath`"" -PassThru
}
Write-Host "Windows Sandbox process id: $($process.Id)"
