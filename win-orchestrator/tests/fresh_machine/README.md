# Fresh-machine acceptance suite

These scripts verify the central guarantee: the single compiled
`winorchestrator.exe` works on a completely clean Windows installation with no
Python, Node.js, Java, Git or build tools pre-installed.

## What is tested

1. The binary starts and prints its version.
2. Deterministic `inspect` on four fixtures:
   - modern Python (`match`/`case`, walrus, f-string, `requirements.txt`, README)
   - modern Node.js (optional chaining, nullish coalescing, global `fetch`,
     `package.json` scripts)
   - modern Java (records, `var`, `List.of`, Maven release 21)
   - an undocumented repository with no manifest or README
3. Two runs produce byte-identical `plan` JSON.
4. `doctor`, `list` and `net-tree` produce valid JSON.
5. When no compatible host runtime exists, `run` fetches a minimal Python
   build, verifies its SHA-256, runs the project under a Job Object, writes a
   BOM-free UTF-8 restore file, and discards the sandbox.
6. `restore` rebuilds the same environment from that file and runs it again.
7. The original fixture directory is never modified.

## Host run

This is the quickest check on a development machine. It uses host injection
when possible; add `-Fetch` to force a real download and checksum verification.

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\run-acceptance.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File .\run-acceptance.ps1 -Fetch
```

Parameters:

| Parameter | Meaning |
| --- | --- |
| `-Binary <path>` | Use an existing binary instead of `target\release\winorchestrator.exe` |
| `-Fetch` | Force a minimal download even when the host has a compatible runtime |
| `-NoCleanPath` | Skip the analysis pass that hides developer tools from `PATH` |

The script builds the release binary if needed.

## Windows Sandbox run

Windows Sandbox starts from a clean Windows image. Enable the optional feature
first if necessary:

```powershell
Enable-WindowsOptionalFeature -Online -FeatureName Containers-DisposableClientVM -All
# Reboot, then:
powershell -NoProfile -ExecutionPolicy Bypass -File .\enter-sandbox.ps1
```

`enter-sandbox.ps1`:

1. Builds `target\release\winorchestrator.exe`.
2. Generates a temporary `.wsb` configuration.
3. Maps the release folder, fixtures and these scripts read-only.
4. Runs `inside-sandbox.ps1` as the logon command.

Use `-DryRun` to generate and validate the `.wsb` configuration without starting
the sandbox:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\enter-sandbox.ps1 -NoBuild -DryRun
```

Results are written to the sandbox desktop as
`winorchestrator-sandbox-<timestamp>.log` and as restore files. The sandbox
needs network access for the first fetch.

## Expected output

The host script ends with:

```text
ACCEPTANCE SUITE PASSED
```

The sandbox script ends with:

```text
FRESH-MACHINE ACCEPTANCE PASSED
```

## Manual clean-room alternative

If Windows Sandbox is unavailable, use any disposable Windows VM with no
developer tooling. Copy only `winorchestrator.exe` and this directory, then run:

```powershell
.\run-acceptance.ps1 -Binary .\winorchestrator.exe -Fetch
```

`-Fetch` is required because a clean VM has no compatible host Python.
