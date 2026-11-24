#requires -Version 5.1
<#
.SYNOPSIS
    Runs inside a Windows Sandbox with no developer runtimes installed.

.DESCRIPTION
    This script is launched by enter-sandbox.ps1. It proves the fresh-machine
    guarantee: the single compiled binary analyses unfamiliar repositories,
    fetches a minimal runtime with SHA-256 verification, runs the project in an
    isolated sandbox, and rebuilds it from a strict UTF-8 restore file.

    The mapped folders are read-only, so all results are written to the
    sandbox desktop.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Binary,
    [Parameter(Mandatory = $true)][string]$Fixtures,
    [string]$OutputDirectory = (Join-Path $env:USERPROFILE 'Desktop')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$logPath = Join-Path $OutputDirectory "winorchestrator-sandbox-$stamp.log"
Start-Transcript -Path $logPath -Force | Out-Null

function Write-Step([string]$Text) {
    Write-Host ""
    Write-Host "== $Text" -ForegroundColor Cyan
}

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "ASSERTION FAILED: $Message" }
}

function Assert-Contains([string]$Text, [string]$Needle, [string]$Label) {
    if (-not $Text.Contains($Needle)) {
        throw "ASSERTION FAILED: $Label did not contain '$Needle'. Actual output:`n$Text"
    }
}

function Invoke-Wo {
    param(
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$Label
    )
    $stderrFile = [System.IO.Path]::GetTempFileName()
    try {
        $stdout = & $Binary @Arguments 2> $stderrFile
        $code = $LASTEXITCODE
        $stderr = if (Test-Path $stderrFile) { Get-Content -Raw -ErrorAction SilentlyContinue $stderrFile } else { '' }
        $stdoutText = ($stdout | Out-String).TrimEnd()
        if ($code -ne 0) {
            throw "$Label failed with exit code $code.`nSTDOUT:`n$stdoutText`nSTDERR:`n$stderr"
        }
        return [pscustomobject]@{ Stdout = $stdoutText; Stderr = $stderr }
    }
    finally {
        Remove-Item -Force -ErrorAction SilentlyContinue $stderrFile
    }
}

function Get-Requirement($Plan, [string]$Runtime) {
    $matches = @($Plan.requirements | Where-Object { $_.runtime -eq $Runtime })
    if ($matches.Count -eq 0) { throw "no $Runtime requirement was derived" }
    return ($matches | Sort-Object { [version]$_.minimum } -Descending | Select-Object -First 1)
}

$exitCode = 1
try {
    Write-Host "WinOrchestrator fresh-machine acceptance"
    Write-Host "  binary:   $Binary"
    Write-Host "  fixtures: $Fixtures"
    Write-Host "  log:      $logPath"
    Assert-True (Test-Path $Binary) "binary missing at $Binary"

    Write-Step 'Confirming this is a fresh sandbox'
    foreach ($tool in @('python.exe', 'node.exe', 'java.exe', 'git.exe')) {
        $found = Get-Command $tool -ErrorAction SilentlyContinue
        if ($null -ne $found -and $null -ne $found.Source -and $found.Source -notlike '*\WindowsApps\*') {
            Write-Host "  note: $tool is already present at $($found.Source)" -ForegroundColor Yellow
        } else {
            Write-Host "  absent: $tool"
        }
    }

    Write-Step 'Version and deterministic analysis'
    $version = Invoke-Wo -Arguments @('--version') -Label 'version'
    Assert-Contains $version.Stdout 'winorchestrator' 'version banner'

    $pythonFixture = Join-Path $Fixtures 'python-service'
    $inspect = (Invoke-Wo -Arguments @('inspect', $pythonFixture, '--json') -Label 'inspect python').Stdout | ConvertFrom-Json
    Assert-True (@($inspect.plan.languages) -contains 'python') 'Python was not detected'
    $requirement = Get-Requirement $inspect.plan 'python'
    Assert-True ([version]$requirement.minimum -ge [version]'3.10.0') "Python floor was $($requirement.minimum), expected >= 3.10.0"

    $node = (Invoke-Wo -Arguments @('inspect', (Join-Path $Fixtures 'node-service'), '--json') -Label 'inspect node').Stdout | ConvertFrom-Json
    $nodeRequirement = Get-Requirement $node.plan 'node'
    Assert-True ([version]$nodeRequirement.minimum -ge [version]'18.0.0') "Node floor was $($nodeRequirement.minimum), expected >= 18.0.0"

    $java = (Invoke-Wo -Arguments @('inspect', (Join-Path $Fixtures 'java-service'), '--json') -Label 'inspect java').Stdout | ConvertFrom-Json
    $javaRequirement = Get-Requirement $java.plan 'java'
    Assert-True ([version]$javaRequirement.minimum -ge [version]'16.0.0') "Java floor was $($javaRequirement.minimum), expected >= 16.0.0"

    Write-Step 'Minimal fetch + isolated run + discard'
    $snapshot = Join-Path $OutputDirectory "python-service-$stamp.envspec.utf8"
    $run = Invoke-Wo -Arguments @(
        'run', $pythonFixture,
        '--discard', '--yes', '--no-setup',
        '--timeout', '300',
        '--snapshot-out', $snapshot
    ) -Label 'sandbox run'
    Assert-Contains $run.Stdout 'python-service says hello to acceptance: ready' 'sandbox stdout'
    Assert-Contains $run.Stdout 'restore file' 'sandbox summary'
    Assert-True (Test-Path $snapshot) 'restore file was not written'

    $bytes = [System.IO.File]::ReadAllBytes($snapshot)
    $hasBom = $bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF
    Assert-True (-not $hasBom) 'restore file must be strict UTF-8 without a BOM'
    $text = [System.IO.File]::ReadAllText($snapshot, (New-Object System.Text.UTF8Encoding($false)))
    Assert-Contains $text 'repo_name = "python-service"' 'restore file'
    Assert-Contains $text '[requirements]' 'restore file'

    Write-Step 'Deterministic rebuild from the restore file'
    $snapshot2 = Join-Path $OutputDirectory "python-service-restored-$stamp.envspec.utf8"
    $restore = Invoke-Wo -Arguments @(
        'restore', $snapshot,
        '--discard', '--yes', '--no-setup', '--run',
        '--timeout', '300',
        '--snapshot-out', $snapshot2
    ) -Label 'restore'
    Assert-Contains $restore.Stdout 'python-service says hello to acceptance: ready' 'restored stdout'
    Assert-True (Test-Path $snapshot2) 'the restored run did not refresh the restore file'

    Write-Step 'Confirming teardown'
    $list = (Invoke-Wo -Arguments @('list', '--json') -Label 'list after teardown').Stdout | ConvertFrom-Json
    $left = @($list | Where-Object { $_.repo_name -eq 'python-service' })
    Assert-True ($left.Count -eq 0) "sandboxes survived discard: $($left.Count)"

    Write-Host ""
    Write-Host 'FRESH-MACHINE ACCEPTANCE PASSED' -ForegroundColor Green
    $exitCode = 0
}
catch {
    Write-Host ""
    Write-Host "FRESH-MACHINE ACCEPTANCE FAILED: $_" -ForegroundColor Red
    Write-Host $_.ScriptStackTrace
}
finally {
    Stop-Transcript | Out-Null
    Write-Host "Log written to $logPath"
    if ($exitCode -ne 0) {
        Write-Host "Press Enter to close this window."
        [void](Read-Host)
    }
}
exit $exitCode
