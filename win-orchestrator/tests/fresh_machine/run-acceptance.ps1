#requires -Version 5.1
<#
.SYNOPSIS
    Host acceptance suite for the compiled WinOrchestrator binary.

.DESCRIPTION
    Verifies the deterministic analysis pipeline, the CLI surface, the local
    dashboard-facing JSON APIs, and (when a compatible host runtime exists) a
    real isolated sandbox run with UTF-8 restore-file teardown.

    Pass -Fetch to force a minimal runtime download and checksum verification
    even when the host already has a compatible runtime. This needs network
    access and is therefore off by default.

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File .\run-acceptance.ps1

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File .\run-acceptance.ps1 -Fetch
#>
[CmdletBinding()]
param(
    [string]$Binary,
    [switch]$Fetch,
    [switch]$NoCleanPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$WorkspaceRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Fixtures = Join-Path $WorkspaceRoot 'crates\cli\tests\fixtures'
$TempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("winorchestrator-acceptance-" + [System.Diagnostics.Process]::GetCurrentProcess().Id)

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
        $stdout = & $script:BinaryPath @Arguments 2> $stderrFile
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

function Assert-Minimum($Plan, [string]$Runtime, [string]$Minimum, [string]$Label) {
    $requirement = Get-Requirement $Plan $Runtime
    Assert-True ([version]$requirement.minimum -ge [version]$Minimum) `
        "$Label should require $Runtime >= $Minimum but derived $($requirement.minimum) ($($requirement.reason))"
}

if (-not $Binary) {
    $Binary = Join-Path $WorkspaceRoot 'target\release\winorchestrator.exe'
    if (-not (Test-Path $Binary)) {
        Write-Step 'Building release binary'
        Push-Location $WorkspaceRoot
        try {
            & cargo build --release
            if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
        }
        finally { Pop-Location }
    }
} elseif (-not [System.IO.Path]::IsPathRooted($Binary)) {
    $Binary = Join-Path $WorkspaceRoot $Binary
}

Assert-True (Test-Path $Binary) "binary not found at $Binary"
$script:BinaryPath = (Resolve-Path $Binary).Path
New-Item -ItemType Directory -Force -Path $TempRoot | Out-Null

Write-Host "WinOrchestrator acceptance suite"
Write-Host "  binary:   $script:BinaryPath"
Write-Host "  fixtures: $Fixtures"
Write-Host "  temp:     $TempRoot"

try {
    Write-Step 'Version banner'
    $version = Invoke-Wo -Arguments @('--version') -Label 'version'
    Assert-Contains $version.Stdout 'winorchestrator' 'version banner'

    Write-Step 'Deterministic analysis of the Python fixture'
    $pythonFixture = Join-Path $Fixtures 'python-service'
    $inspect = Invoke-Wo -Arguments @('inspect', $pythonFixture, '--json') -Label 'inspect python'
    $python = $inspect.Stdout | ConvertFrom-Json
    Assert-True (@($python.plan.languages) -contains 'python') 'Python language was not detected'
    Assert-Minimum $python.plan 'python' '3.10.0' 'python-service'
    Assert-True (@($python.plan.dependencies | Where-Object { $_.name -eq 'httpx' }).Count -eq 1) 'httpx dependency missing'
    Assert-True (@($python.plan.entrypoints | Where-Object { $_.path -eq 'app.py' }).Count -ge 1) 'app.py entrypoint missing'

    $inspectAgain = Invoke-Wo -Arguments @('inspect', $pythonFixture, '--json') -Label 'inspect python (repeat)'
    $pythonAgain = $inspectAgain.Stdout | ConvertFrom-Json
    $firstPlan = $python.plan | ConvertTo-Json -Depth 50 -Compress
    $secondPlan = $pythonAgain.plan | ConvertTo-Json -Depth 50 -Compress
    Assert-True ($firstPlan -eq $secondPlan) 'two analyses produced different plans'

    Write-Step 'Node.js and Java fixture analysis'
    $node = (Invoke-Wo -Arguments @('inspect', (Join-Path $Fixtures 'node-service'), '--json') -Label 'inspect node').Stdout | ConvertFrom-Json
    Assert-True (@($node.plan.languages) -contains 'javascript') 'JavaScript was not detected'
    Assert-Minimum $node.plan 'node' '18.0.0' 'node-service'

    $java = (Invoke-Wo -Arguments @('inspect', (Join-Path $Fixtures 'java-service'), '--json') -Label 'inspect java').Stdout | ConvertFrom-Json
    Assert-True (@($java.plan.languages) -contains 'java') 'Java was not detected'
    Assert-Minimum $java.plan 'java' '16.0.0' 'java-service'
    Assert-True (@($java.plan.entrypoints | Where-Object { $_.kind -eq 'java_main' -and $_.path -eq 'com.example.demo.Main' }).Count -eq 1) 'Java main class was not detected'

    Write-Step 'Undocumented repository analysis'
    $undocumented = (Invoke-Wo -Arguments @('inspect', (Join-Path $Fixtures 'undocumented-python'), '--json') -Label 'inspect undocumented').Stdout | ConvertFrom-Json
    Assert-True (@($undocumented.plan.manifests).Count -eq 0) 'undocumented fixture unexpectedly has a manifest'
    Assert-Minimum $undocumented.plan 'python' '3.8.0' 'undocumented-python'

    Write-Step 'Doctor, list and net-tree JSON'
    $doctor = (Invoke-Wo -Arguments @('doctor', '--json') -Label 'doctor').Stdout | ConvertFrom-Json
    Assert-True ($null -ne $doctor.profile) 'doctor did not return an execution profile'
    $list = (Invoke-Wo -Arguments @('list', '--json') -Label 'list').Stdout
    Assert-True ($list.TrimStart().StartsWith('[')) 'list --json did not return an array'
    $tree = (Invoke-Wo -Arguments @('net-tree', '--all', '--json') -Label 'net-tree').Stdout | ConvertFrom-Json
    Assert-True ($null -ne $tree.nodes) 'net-tree did not return process nodes'

    if (-not $NoCleanPath) {
        Write-Step 'Analysis with host developer tools hidden from PATH'
        $originalPath = $env:PATH
        try {
            $env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"
            $clean = (Invoke-Wo -Arguments @('inspect', $pythonFixture, '--json') -Label 'inspect with clean PATH').Stdout | ConvertFrom-Json
            Assert-Minimum $clean.plan 'python' '3.10.0' 'python-service (clean PATH)'
        }
        finally {
            $env:PATH = $originalPath
        }
    }

    $hostPython = @($python.matches | Where-Object { $_.runtime -eq 'python' -and $_.status -eq 'compatible' })
    if ($Fetch -or $hostPython.Count -gt 0) {
        Write-Step 'Isolated run, UTF-8 snapshot and clean teardown'
        $snapshot = Join-Path $TempRoot 'python-service.envspec.utf8'
        $runArgs = @('run', $pythonFixture, '--discard', '--yes', '--no-setup', '--timeout', '300', '--snapshot-out', $snapshot)
        if ($Fetch) {
            $runArgs += '--force-download'
        } else {
            $runArgs += '--offline'
        }
        $run = Invoke-Wo -Arguments $runArgs -Label 'sandbox run'
        Assert-Contains $run.Stdout 'python-service says hello to acceptance: ready' 'sandbox stdout'
        Assert-Contains $run.Stdout 'restore file' 'sandbox summary'
        Assert-True (Test-Path $snapshot) 'restore file was not written'

        $bytes = [System.IO.File]::ReadAllBytes($snapshot)
        $hasBom = $bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF
        Assert-True (-not $hasBom) 'restore file must be strict UTF-8 without a BOM'
        $text = [System.IO.File]::ReadAllText($snapshot, (New-Object System.Text.UTF8Encoding($false)))
        Assert-Contains $text 'repo_name = "python-service"' 'restore file'
        Assert-Contains $text '[requirements]' 'restore file'

        Assert-True (-not (Test-Path (Join-Path $pythonFixture '.venv'))) 'the original repository gained a .venv directory'
        Assert-True (-not (Test-Path (Join-Path $pythonFixture 'sandbox.json'))) 'the original repository gained a sandbox record'
    } else {
        Write-Step 'Skipping sandbox execution (no compatible host Python; use -Fetch to download one)'
    }

    Write-Host ""
    Write-Host 'ACCEPTANCE SUITE PASSED' -ForegroundColor Green
}
finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $TempRoot
}
