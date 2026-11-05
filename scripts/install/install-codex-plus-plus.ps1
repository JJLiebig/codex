param(
    [string]$TargetExe,
    [string]$PythonExe,
    [switch]$ResumeUpgrade,

    [string]$ShimDir = (Join-Path $env:LOCALAPPDATA "Programs\CodexPlusPlus\bin"),

    [switch]$Install,
    [switch]$Remove,
    [switch]$AddToUserPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$pendingPath = Join-Path $ShimDir '.codex-plus-plus-pending.json'
$pending = $null
if ($ResumeUpgrade) {
    if (-not (Test-Path -LiteralPath $pendingPath)) { exit 0 }
    $pending = Get-Content -LiteralPath $pendingPath -Raw | ConvertFrom-Json
    $TargetExe = $pending.TargetExe
    if ($PythonExe) { $pending.PythonExe = $PythonExe }
    else { $PythonExe = $pending.PythonExe }
    $env:CODEX_HOME = $pending.CodexHome
    $AddToUserPath = $pending.AddToUserPath
    $Install = $true
}

if (-not ("CodexPlusPlus.PhysicalPath" -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace CodexPlusPlus {
    public static class PhysicalPath {
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern SafeFileHandle CreateFile(
            string fileName,
            uint desiredAccess,
            uint shareMode,
            IntPtr securityAttributes,
            uint creationDisposition,
            uint flagsAndAttributes,
            IntPtr templateFile
        );

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern uint GetFinalPathNameByHandle(
            SafeFileHandle file,
            StringBuilder path,
            uint pathLength,
            uint flags
        );

        public static string Resolve(string path) {
            using (var handle = CreateFile(path, 0, 7, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero)) {
                if (handle.IsInvalid) {
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                }
                var resolved = new StringBuilder(32768);
                uint length = GetFinalPathNameByHandle(handle, resolved, (uint)resolved.Capacity, 0);
                if (length == 0 || length >= resolved.Capacity) {
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                }
                string value = resolved.ToString();
                if (value.StartsWith(@"\\?\UNC\", StringComparison.OrdinalIgnoreCase)) {
                    return @"\\" + value.Substring(8);
                }
                return value.StartsWith(@"\\?\", StringComparison.OrdinalIgnoreCase)
                    ? value.Substring(4)
                    : value;
            }
        }
    }
}
'@
}

function Resolve-FullPath {
    param([string]$Path)
    $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
}

function Resolve-PhysicalPath {
    param([string]$Path)

    $fullPath = Resolve-FullPath -Path $Path
    $suffix = [System.Collections.Generic.List[string]]::new()
    $existingPath = $fullPath
    while (-not (Test-Path -LiteralPath $existingPath)) {
        $suffix.Insert(0, (Split-Path -Leaf $existingPath))
        $parent = Split-Path -Parent $existingPath
        if ([string]::IsNullOrWhiteSpace($parent) -or $parent -eq $existingPath) {
            throw "Could not resolve an existing parent for path: $fullPath"
        }
        $existingPath = $parent
    }
    $physicalPath = [CodexPlusPlus.PhysicalPath]::Resolve($existingPath)
    foreach ($component in $suffix) {
        $physicalPath = Join-Path $physicalPath $component
    }
    return [System.IO.Path]::GetFullPath($physicalPath)
}

function Move-ToPathFront {
    param(
        [string]$PathValue,
        [string]$Entry
    )

    $needle = $Entry.TrimEnd("\")
    $segments = @()
    if (-not [string]::IsNullOrWhiteSpace($PathValue)) {
        foreach ($segment in $PathValue.Split(";", [System.StringSplitOptions]::RemoveEmptyEntries)) {
            if ($segment.TrimEnd("\") -ine $needle) {
                $segments += $segment
            }
        }
    }

    (@($Entry) + $segments) -join ";"
}

function Get-Sha256 {
    param([string]$Path)

    $stream = [System.IO.File]::OpenRead($Path)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        return ([System.BitConverter]::ToString($sha256.ComputeHash($stream))).Replace("-", "")
    } finally {
        $sha256.Dispose()
        $stream.Dispose()
    }
}

function Invoke-WithInstallLocks {
    param(
        [string[]]$LockPaths,
        [scriptblock]$Script
    )

    $deadline = [DateTime]::UtcNow.AddSeconds(60)
    $locks = @()
    try {
        foreach ($lockPath in @($LockPaths | Sort-Object -Unique)) {
            $lock = $null
            while ($null -eq $lock) {
                try {
                    $lock = [System.IO.File]::Open(
                        $lockPath,
                        [System.IO.FileMode]::OpenOrCreate,
                        [System.IO.FileAccess]::ReadWrite,
                        [System.IO.FileShare]::None
                    )
                } catch [System.IO.IOException] {
                    if ([DateTime]::UtcNow -ge $deadline) {
                        throw "Timed out waiting for the Codex++ install lock: $lockPath"
                    }
                    Start-Sleep -Milliseconds 100
                }
            }
            $locks += $lock
        }
        & $Script
    } finally {
        foreach ($lock in $locks) {
            $lock.Dispose()
        }
    }
}

function Test-IsJunction {
    param([object]$Item)

    return $null -ne $Item -and
        ($Item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -and
        $Item.LinkType -eq "Junction"
}

function Test-CmdLeaseActive {
    param([System.IO.FileInfo]$Lease)

    if ($Lease.Name -notmatch "^cmd\.(\d+)\.(\d+)\.lease$") {
        return $false
    }
    try {
        $process = [System.Diagnostics.Process]::GetProcessById([int]$Matches[1])
        return $process.StartTime.ToUniversalTime().Ticks -eq [long]$Matches[2]
    } catch {
        return $false
    }
}

function Remove-StaleReleases {
    param(
        [string]$ReleasesDir,
        [string]$GenerationLinksDir,
        [string]$GenerationLeasesDir,
        [string[]]$KeepReleaseDirs
    )

    if (-not (Test-Path -LiteralPath $ReleasesDir -PathType Container)) {
        return
    }
    foreach ($release in Get-ChildItem -LiteralPath $ReleasesDir -Directory -Force -ErrorAction SilentlyContinue) {
        if ($release.Name.StartsWith(".staging.")) {
            Remove-Item -LiteralPath $release.FullName -Recurse -Force -ErrorAction SilentlyContinue
            continue
        }
        $keepRelease = $false
        foreach ($keepReleaseDir in $KeepReleaseDirs) {
            if (-not [string]::IsNullOrWhiteSpace($keepReleaseDir) -and
                $release.FullName.Equals($keepReleaseDir, [System.StringComparison]::OrdinalIgnoreCase)) {
                $keepRelease = $true
                break
            }
        }
        if ($keepRelease) {
            continue
        }
        $generationLeaseDir = Join-Path $GenerationLeasesDir $release.Name
        $pruningGatePath = Join-Path $generationLeaseDir ".pruning"
        $prunedPath = Join-Path $GenerationLeasesDir "$($release.Name).pruned"
        New-Item -ItemType Directory -Force -Path $generationLeaseDir | Out-Null
        Remove-Item -LiteralPath $pruningGatePath -Force -ErrorAction SilentlyContinue
        $pruningGate = $null
        $powershellLease = $null
        try {
            $pruningGate = [System.IO.File]::Open(
                $pruningGatePath,
                [System.IO.FileMode]::CreateNew,
                [System.IO.FileAccess]::ReadWrite,
                [System.IO.FileShare]::None
            )
        } catch [System.IO.IOException] {
            Write-Host "==> Kept active Codex++ release at $($release.FullName)"
            continue
        }
        try {
            $powershellLease = [System.IO.File]::Open(
                (Join-Path $generationLeaseDir "powershell.lock"),
                [System.IO.FileMode]::OpenOrCreate,
                [System.IO.FileAccess]::ReadWrite,
                [System.IO.FileShare]::None
            )
            $activeCmdLease = $false
            foreach ($cmdLease in Get-ChildItem -LiteralPath $generationLeaseDir -Filter "cmd.*.lease" -File -Force) {
                if (Test-CmdLeaseActive -Lease $cmdLease) {
                    $activeCmdLease = $true
                } else {
                    Remove-Item -LiteralPath $cmdLease.FullName -Force -ErrorAction SilentlyContinue
                }
            }
            if ($activeCmdLease) {
                Write-Host "==> Kept active Codex++ release at $($release.FullName)"
                continue
            }
            $releaseTargetPath = Join-Path $release.FullName "bin\codex.exe"
            if (Test-Path -LiteralPath $releaseTargetPath -PathType Leaf) {
                Remove-Item -LiteralPath $releaseTargetPath -Force
            }
            Remove-Item -LiteralPath $release.FullName -Recurse -Force
            $generationLink = Get-Item -LiteralPath (Join-Path $GenerationLinksDir $release.Name) -Force -ErrorAction SilentlyContinue
            if (Test-IsJunction -Item $generationLink) {
                $generationLink.Delete()
            }
            Write-Host "==> Removed stale Codex++ release at $($release.FullName)"
        } catch [System.IO.IOException] {
            Write-Host "==> Kept active Codex++ release at $($release.FullName)"
        } catch {
            Write-Warning "Could not remove stale Codex++ release at $($release.FullName): $($_.Exception.Message)"
        } finally {
            if ($null -ne $powershellLease) {
                $powershellLease.Dispose()
            }
            $pruningGate.Dispose()
            if (Test-Path -LiteralPath $release.FullName -PathType Container) {
                Remove-Item -LiteralPath $pruningGatePath -Force -ErrorAction SilentlyContinue
            } else {
                [System.IO.File]::WriteAllBytes($prunedPath, [byte[]]::new(0))
                Remove-Item -LiteralPath $generationLeaseDir -Recurse -Force -ErrorAction SilentlyContinue
            }
        }
    }
}

$ShimDir = Resolve-PhysicalPath -Path $ShimDir
$shimPath = Join-Path $ShimDir "codex.ps1"
$cmdShimPath = Join-Path $ShimDir "codex.cmd"
$targetPointerPath = Join-Path $ShimDir ".codex-plus-plus-current"
$generationLinksDir = Join-Path $ShimDir ".codex-plus-plus-generations"
$generationLeasesDir = Join-Path $ShimDir ".codex-plus-plus-leases"
$markerPath = Join-Path $ShimDir ".codex-plus-plus-shim"
$codexHome = if ([string]::IsNullOrWhiteSpace($env:CODEX_HOME)) {
    Resolve-PhysicalPath -Path (Join-Path $env:USERPROFILE ".codex")
} else {
    Resolve-PhysicalPath -Path $env:CODEX_HOME
}
$installRoot = Resolve-PhysicalPath -Path (Join-Path $codexHome "packages\codex-plus-plus")
$releasesRoot = Resolve-PhysicalPath -Path (Join-Path $installRoot "releases")
$sha256 = [System.Security.Cryptography.SHA256]::Create()
try {
    $shimLockId = ([System.BitConverter]::ToString(
        $sha256.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($ShimDir.ToUpperInvariant()))
    )).Replace("-", "")
} finally {
    $sha256.Dispose()
}
$releasesDir = Join-Path $releasesRoot $shimLockId
$releaseLockPath = Join-Path $installRoot "install.lock"
$shimLockPath = Join-Path (Split-Path -Parent $ShimDir) ".codex-plus-plus-install-$shimLockId.lock"
$installLockPaths = @($releaseLockPath, $shimLockPath)

if ($Remove) {
    foreach ($lockPath in $installLockPaths) {
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $lockPath) | Out-Null
    }
    Invoke-WithInstallLocks -LockPaths $installLockPaths -Script {
        if (Test-Path -LiteralPath $pendingPath) { throw "Finish the pending Codex upgrade before removing this installation." }
        if (Test-Path -LiteralPath $shimPath -PathType Leaf) {
            Remove-Item -LiteralPath $shimPath
            Write-Host "==> Removed shim at $shimPath"
        } else {
            Write-Host "==> No shim found at $shimPath"
        }
        $ownsCompanion = (Test-Path -LiteralPath $cmdShimPath -PathType Leaf) -and
            (Test-Path -LiteralPath $markerPath -PathType Leaf) -and
            (Get-Content -LiteralPath $markerPath -Raw).Trim() -ceq (Get-Sha256 -Path $cmdShimPath)
        if ($ownsCompanion) {
            if (Test-Path -LiteralPath $cmdShimPath -PathType Leaf) {
                Remove-Item -LiteralPath $cmdShimPath
                Write-Host "==> Removed shim at $cmdShimPath"
            }
            Remove-Item -LiteralPath $targetPointerPath -Force -ErrorAction SilentlyContinue
            Remove-Item -LiteralPath $markerPath
        } elseif (Test-Path -LiteralPath $cmdShimPath -PathType Leaf) {
            Write-Host "==> Kept non-shim launcher at $cmdShimPath"
        } else {
            Write-Host "==> No shim found at $cmdShimPath"
        }
    }
    exit 0
}

if ([string]::IsNullOrWhiteSpace($TargetExe)) {
    Write-Error "-TargetExe is required unless -Remove is set."
    exit 2
}

$targetPath = Resolve-PhysicalPath -Path $TargetExe
if ($targetPath.StartsWith("\\")) {
    Write-Error "Target fork executable must be on a local filesystem: $targetPath"
    exit 1
}
$activeCodex = Get-Command codex -ErrorAction SilentlyContinue
$activePath = if ($activeCodex) { $activeCodex.Source } else { "not found on PATH" }

Write-Host "==> Codex++ shim"
Write-Host "Active codex path: $activePath"
Write-Host "Shim paths: $shimPath, $cmdShimPath"
Write-Host "Target fork executable: $targetPath"
Write-Host "Target reachable: $(Test-Path -LiteralPath $targetPath -PathType Leaf)"

if (-not $Install) {
    Write-Host "==> Dry run only; pass -Install to write the shim."
    exit 0
}

if (-not (Test-Path -LiteralPath $targetPath -PathType Leaf)) {
    Write-Error "Target fork executable does not exist: $targetPath"
    exit 1
}
$targetFileName = [System.IO.Path]::GetFileName($targetPath)
if ($targetFileName -ine "codex.exe") {
    Write-Error "Target fork executable must be named codex.exe: $targetPath"
    exit 1
}
$targetBinDir = Split-Path -Parent $targetPath
if ((Split-Path -Leaf $targetBinDir) -ine "bin") {
    Write-Error "Target fork executable must be inside a package bin directory: $targetPath"
    exit 1
}
$packageDir = Split-Path -Parent $targetBinDir
if (-not (Test-Path -LiteralPath (Join-Path $packageDir "codex-package.json") -PathType Leaf)) {
    Write-Error "Target fork executable must belong to a Codex package: $targetPath"
    exit 1
}
$packageDirPrefix = $packageDir.TrimEnd("\") + "\"
foreach ($managedPath in @($ShimDir, $releasesRoot)) {
    if ($managedPath.Equals($packageDir, [System.StringComparison]::OrdinalIgnoreCase) -or
        $managedPath.StartsWith($packageDirPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        Write-Error "Codex++ managed install paths must be outside the source package: $packageDir"
        exit 1
    }
}
$shimDirPrefix = $ShimDir.TrimEnd("\") + "\"
$releasesRootPrefix = $releasesRoot.TrimEnd("\") + "\"
if ($ShimDir.Equals($releasesRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
    $ShimDir.StartsWith($releasesRootPrefix, [System.StringComparison]::OrdinalIgnoreCase) -or
    $releasesRoot.StartsWith($shimDirPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    Write-Error "Codex++ shim and release directories must not overlap."
    exit 1
}
$cmdContent = @"
@echo off
setlocal DisableDelayedExpansion
if exist "%~dp0.codex-plus-plus-pending.json" (
  set "CODEX_PLUS_PLUS_FINISH_ONLY=1"
  "%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "%~dp0codex.ps1"
  if errorlevel 1 exit /b 1
  set "CODEX_PLUS_PLUS_FINISH_ONLY="
)
for %%I in ("%~dp0.") do set "CODEX_PLUS_PLUS_SHIM_DIR=%%~fI"
set "CODEX_PLUS_PLUS_IDENTITY_FILE=%~dp0.codex-plus-plus-leases\.identity.%RANDOM%%RANDOM%%RANDOM%"
set "CODEX_PLUS_PLUS_PARENT_IDENTITY=1"
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -InputFormat None -ExecutionPolicy Bypass -File "%~dp0codex.ps1" >"%CODEX_PLUS_PLUS_IDENTITY_FILE%"
set "CODEX_PLUS_PLUS_PARENT_IDENTITY="
set "CODEX_PLUS_PLUS_IDENTITY="
set /p "CODEX_PLUS_PLUS_IDENTITY="<"%CODEX_PLUS_PLUS_IDENTITY_FILE%"
del /q "%CODEX_PLUS_PLUS_IDENTITY_FILE%" >nul 2>&1
for /f "tokens=1" %%A in ("%CODEX_PLUS_PLUS_IDENTITY%") do set "CODEX_PLUS_PLUS_PID=%%A"
for /f "tokens=2" %%A in ("%CODEX_PLUS_PLUS_IDENTITY%") do set "CODEX_PLUS_PLUS_STARTED=%%A"
if not defined CODEX_PLUS_PLUS_STARTED exit /b 1
:codex_plus_plus_retry
set "CODEX_PLUS_PLUS_GENERATION="
set /p "CODEX_PLUS_PLUS_GENERATION="<"%~dp0.codex-plus-plus-current"
if not defined CODEX_PLUS_PLUS_GENERATION exit /b 1
set "CODEX_PLUS_PLUS_LEASE_DIR=%~dp0.codex-plus-plus-leases\%CODEX_PLUS_PLUS_GENERATION%"
if exist "%CODEX_PLUS_PLUS_LEASE_DIR%\.pruning" goto codex_plus_plus_retry
if exist "%~dp0.codex-plus-plus-leases\%CODEX_PLUS_PLUS_GENERATION%.pruned" goto codex_plus_plus_retry
set "CODEX_PLUS_PLUS_LEASE=%CODEX_PLUS_PLUS_LEASE_DIR%\cmd.%CODEX_PLUS_PLUS_PID%.%CODEX_PLUS_PLUS_STARTED%.lease"
type nul >"%CODEX_PLUS_PLUS_LEASE%" 2>nul
if errorlevel 1 goto codex_plus_plus_retry
if exist "%CODEX_PLUS_PLUS_LEASE_DIR%\.pruning" goto codex_plus_plus_release_and_retry
if exist "%~dp0.codex-plus-plus-leases\%CODEX_PLUS_PLUS_GENERATION%.pruned" goto codex_plus_plus_release_and_retry
if not exist "%~dp0.codex-plus-plus-generations\%CODEX_PLUS_PLUS_GENERATION%\$targetFileName" goto codex_plus_plus_release_and_retry
"%~dp0.codex-plus-plus-generations\%CODEX_PLUS_PLUS_GENERATION%\$targetFileName" %*
set "CODEX_PLUS_PLUS_EXIT=%ERRORLEVEL%"
del /q "%CODEX_PLUS_PLUS_LEASE%" >nul 2>&1
exit /b %CODEX_PLUS_PLUS_EXIT%
:codex_plus_plus_release_and_retry
del /q "%CODEX_PLUS_PLUS_LEASE%" >nul 2>&1
goto codex_plus_plus_retry
"@
foreach ($lockPath in $installLockPaths) {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $lockPath) | Out-Null
}
Invoke-WithInstallLocks -LockPaths $installLockPaths -Script {
    if ($ResumeUpgrade -and -not (Test-Path -LiteralPath $pendingPath)) { return }
    if (-not $ResumeUpgrade -and (Test-Path -LiteralPath $pendingPath)) {
        Write-Host "Upgrade pending. Close existing Codex sessions, then launch Codex to finish."
        exit 75
    }
    $historyTools = Join-Path $packageDir 'codex-resources\history-upgrade'
    $upgrade = $null
    if (Test-Path -LiteralPath (Join-Path $historyTools 'windows_upgrade.ps1')) {
        . (Join-Path $historyTools 'windows_upgrade.ps1')
        if (-not $ResumeUpgrade) {
            $upgrade = Get-HistoryUpgrade -Target $targetPath -Python $PythonExe -Helper (Join-Path $historyTools 'windows_upgrade.py')
            if (-not $upgrade.Required -or (-not (Test-Path -LiteralPath $upgrade.Database) -and
                -not (Test-Path -LiteralPath $targetPointerPath))) { $upgrade = $null }
        }
    }
    if (($upgrade -or $ResumeUpgrade) -and (Test-Path -LiteralPath $releasesRoot)) {
        if (@(Get-ChildItem -LiteralPath $releasesRoot -Directory | Where-Object { $_.Name -ne $shimLockId }).Count) {
            throw "History maintenance requires a single managed Codex installation. Installation was left unchanged."
        }
    }
    $existingMarker = Get-Item -LiteralPath $markerPath -Force -ErrorAction SilentlyContinue
    $ownsCompanion = $existingMarker -and
        (Test-Path -LiteralPath $cmdShimPath -PathType Leaf) -and
        (Get-Content -LiteralPath $markerPath -Raw).Trim() -ceq (Get-Sha256 -Path $cmdShimPath)
    if ($existingMarker -and -not $ownsCompanion) {
        throw "Refusing to replace unmanaged marker: $markerPath"
    }
    if ((Test-Path -LiteralPath $cmdShimPath -PathType Leaf) -and -not $ownsCompanion) {
        throw "Refusing to replace non-shim launcher: $cmdShimPath"
    }
    $existingTarget = Get-Item -LiteralPath $targetPointerPath -Force -ErrorAction SilentlyContinue
    $managedTarget = $existingTarget -and -not $existingTarget.PSIsContainer
    if ($existingTarget -and (-not $ownsCompanion -or -not $managedTarget)) {
        throw "Refusing to replace unmanaged target path: $targetPointerPath"
    }

    $previousGeneration = if ($existingTarget) {
        [System.IO.File]::ReadAllText($targetPointerPath).Trim()
    } else {
        $null
    }
    $previousReleaseDir = if ($previousGeneration -match "^\d{8}T\d{13}Z$") {
        Join-Path $releasesDir $previousGeneration
    } else {
        $null
    }

    New-Item -ItemType Directory -Force -Path $ShimDir | Out-Null
    New-Item -ItemType Directory -Force -Path $releasesDir | Out-Null
    New-Item -ItemType Directory -Force -Path $generationLinksDir | Out-Null
    New-Item -ItemType Directory -Force -Path $generationLeasesDir | Out-Null
    $releaseName = if ($ResumeUpgrade) { Split-Path -Leaf $packageDir } else { [DateTime]::UtcNow.ToString(
        "yyyyMMddTHHmmssfffffffZ",
        [System.Globalization.CultureInfo]::InvariantCulture
    ) }
    $releaseDir = Join-Path $releasesDir $releaseName
    $stagingDir = Join-Path $releasesDir ".staging.$releaseName.$PID"
    if (-not $ResumeUpgrade) { try {
        New-Item -ItemType Directory -Path $stagingDir | Out-Null
        Get-ChildItem -LiteralPath $packageDir -Force |
            Copy-Item -Destination $stagingDir -Recurse -Force
        Move-Item -LiteralPath $stagingDir -Destination $releaseDir
    } finally {
        if (Test-Path -LiteralPath $stagingDir) {
            Remove-Item -LiteralPath $stagingDir -Recurse -Force -ErrorAction SilentlyContinue
        }
    } }
    $installedBinDir = Join-Path $releaseDir "bin"
    $generationLinkPath = Join-Path $generationLinksDir $releaseName
    if (-not $ResumeUpgrade) { New-Item -ItemType Junction -Path $generationLinkPath -Target $installedBinDir | Out-Null }
    $generationLeaseDir = Join-Path $generationLeasesDir $releaseName
    if (-not $ResumeUpgrade) { New-Item -ItemType Directory -Path $generationLeaseDir | Out-Null }
    if (-not $ResumeUpgrade) { [System.IO.File]::WriteAllBytes(
        (Join-Path $generationLeaseDir "powershell.lock"),
        [byte[]]::new(0)
    ) }
    $installedTargetPath = Join-Path $generationLinkPath $targetFileName
    $content = @"
if (`$env:CODEX_PLUS_PLUS_PARENT_IDENTITY -ne '1') {
    `$pendingPath = Join-Path `$PSScriptRoot '.codex-plus-plus-pending.json'
    if (Test-Path -LiteralPath `$pendingPath) {
        `$pending = Get-Content -LiteralPath `$pendingPath -Raw | ConvertFrom-Json
        & (Join-Path `$env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe') -NoProfile -ExecutionPolicy Bypass -File `$pending.Installer -ShimDir `$PSScriptRoot -ResumeUpgrade
        if (`$LASTEXITCODE -ne 0) { exit `$LASTEXITCODE }
    }
    if (`$env:CODEX_PLUS_PLUS_FINISH_ONLY -eq '1') { exit 0 }
}
if (`$env:CODEX_PLUS_PLUS_PARENT_IDENTITY -eq '1') {
    Add-Type -TypeDefinition @'
using System;
using System.Diagnostics;
using System.Runtime.InteropServices;

namespace CodexPlusPlus {
    [StructLayout(LayoutKind.Sequential)]
    public struct ProcessBasicInformation {
        public IntPtr Reserved1;
        public IntPtr PebBaseAddress;
        public IntPtr Reserved2_0;
        public IntPtr Reserved2_1;
        public IntPtr UniqueProcessId;
        public IntPtr InheritedFromUniqueProcessId;
    }

    public static class NativeMethods {
        [DllImport("ntdll.dll")]
        private static extern int NtQueryInformationProcess(
            IntPtr processHandle,
            int processInformationClass,
            ref ProcessBasicInformation processInformation,
            int processInformationLength,
            out int returnLength
        );

        public static int GetParentProcessId() {
            var information = new ProcessBasicInformation();
            int returnLength;
            int status = NtQueryInformationProcess(
                Process.GetCurrentProcess().Handle,
                0,
                ref information,
                Marshal.SizeOf(information),
                out returnLength
            );
            if (status != 0) {
                throw new InvalidOperationException("NtQueryInformationProcess failed: " + status);
            }
            return information.InheritedFromUniqueProcessId.ToInt32();
        }
    }
}
'@
    `$parentProcessId = [CodexPlusPlus.NativeMethods]::GetParentProcessId()
    `$parent = [System.Diagnostics.Process]::GetProcessById(`$parentProcessId)
    [Console]::Out.WriteLine("{0} {1}", `$parent.Id, `$parent.StartTime.ToUniversalTime().Ticks)
    exit 0
}
`$env:CODEX_PLUS_PLUS_SHIM_DIR = `$PSScriptRoot
while (`$true) {
    `$generation = [System.IO.File]::ReadAllText((Join-Path `$PSScriptRoot '.codex-plus-plus-current')).Trim()
    `$leasePath = Join-Path `$PSScriptRoot ".codex-plus-plus-leases\`$generation\powershell.lock"
    try {
        `$lease = [System.IO.File]::Open(
            `$leasePath,
            [System.IO.FileMode]::Open,
            [System.IO.FileAccess]::Read,
            [System.IO.FileShare]::Read
        )
    } catch [System.IO.IOException] {
        continue
    }
    `$target = Join-Path `$PSScriptRoot ".codex-plus-plus-generations\`$generation\$targetFileName"
    if (Test-Path -LiteralPath `$target -PathType Leaf) {
        break
    }
    `$lease.Dispose()
}
try {
    if (`$MyInvocation.ExpectingInput) {
        `$input | & `$target @args
    } else {
        & `$target @args
    }
    if (`$null -ne `$global:LASTEXITCODE) { exit `$global:LASTEXITCODE }
} finally {
    `$lease.Dispose()
}
"@
    if (-not $ResumeUpgrade) {
    [System.IO.File]::WriteAllText($shimPath, $content, [System.Text.UTF8Encoding]::new($true))
    [System.IO.File]::WriteAllText($cmdShimPath, $cmdContent, [System.Text.UTF8Encoding]::new($false))
    [System.IO.File]::WriteAllText($markerPath, (Get-Sha256 -Path $cmdShimPath), [System.Text.UTF8Encoding]::new($false))
    }
    if (-not (Test-Path -LiteralPath $installedTargetPath -PathType Leaf)) {
        throw "Installed Codex++ target is not reachable: $installedTargetPath"
    }
    if ($upgrade) {
        $installedTools = Join-Path $releaseDir 'codex-resources\history-upgrade'
        $installer = Join-Path $installedTools 'install.ps1'
        Copy-Item -LiteralPath $PSCommandPath -Destination $installer
        $upgrade += @{ TargetExe = (Join-Path $releaseDir 'bin\codex.exe'); Installer = $installer; CodexHome = $codexHome; AddToUserPath = [bool]$AddToUserPath }
        $temporary = "$pendingPath.$PID.tmp"
        [System.IO.File]::WriteAllText($temporary, ($upgrade | ConvertTo-Json), [System.Text.UTF8Encoding]::new($false))
        Move-Item -LiteralPath $temporary -Destination $pendingPath
        Write-Host "Upgrade pending. Close existing Codex sessions, then run: & '$shimPath'"
        exit 75
    }
    $activate = {
    $targetPointerTempPath = "$targetPointerPath.$PID.tmp"
    $targetPointerBackupPath = "$targetPointerPath.$PID.bak"
    try {
        [System.IO.File]::WriteAllText(
            $targetPointerTempPath,
            $releaseName,
            [System.Text.UTF8Encoding]::new($false)
        )
        if ($existingTarget) {
            [System.IO.File]::Replace(
                $targetPointerTempPath,
                $targetPointerPath,
                $targetPointerBackupPath
            )
        } else {
            Move-Item -LiteralPath $targetPointerTempPath -Destination $targetPointerPath
        }
    } finally {
        Remove-Item -LiteralPath $targetPointerTempPath -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $targetPointerBackupPath -Force -ErrorAction SilentlyContinue
    }
    Write-Host "==> Installed shims at $shimPath and $cmdShimPath"
    Write-Host "==> Active release: $releaseDir"
    }
    if ($ResumeUpgrade) {
        Invoke-HistoryUpgrade -Pending $pending -Leases $generationLeasesDir -Links $generationLinksDir -Releases $releasesDir -Incoming $releaseName -Activate $activate
        Remove-Item -LiteralPath $pendingPath
    } else { & $activate }
    Remove-StaleReleases `
        -ReleasesDir $releasesDir `
        -GenerationLinksDir $generationLinksDir `
        -GenerationLeasesDir $generationLeasesDir `
        -KeepReleaseDirs @($releaseDir, $previousReleaseDir)
}

if ($AddToUserPath) {
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $newUserPath = Move-ToPathFront -PathValue $userPath -Entry $ShimDir
    [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
    $env:Path = Move-ToPathFront -PathValue $env:Path -Entry $ShimDir
    Write-Host "==> Moved $ShimDir to the front of the user PATH for future shells."
}

$currentCodex = Get-Command codex -ErrorAction SilentlyContinue
if ($currentCodex -and @($shimPath, $cmdShimPath) -icontains $currentCodex.Source) {
    Write-Host "==> Run: codex"
} else {
    Write-Host "==> Run now: `$env:Path = `"$ShimDir;`$env:Path`"; codex"
}
