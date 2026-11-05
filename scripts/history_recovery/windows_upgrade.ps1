# Only the managed Windows installer calls these functions, under its install locks.
function Get-HistoryUpgrade {
    param([string]$Target, [string]$Python, [string]$Helper)

    # The new executable only resolves configuration; it must not open a database.
    $database = & $Target debug history-upgrade-target
    if ($LASTEXITCODE -ne 0 -or @($database).Count -ne 1 -or
        -not [System.IO.Path]::IsPathRooted($database)) {
        throw "Cannot resolve history for this upgrade. Installation was left unchanged."
    }
    if (-not (Test-Path -LiteralPath $database -PathType Leaf)) { return $null }
    if ([string]::IsNullOrWhiteSpace($Python)) {
        $Python = $env:Path -split ';' |
            Where-Object { [System.IO.Path]::IsPathRooted($_) } |
            ForEach-Object { Join-Path $_ 'python.exe' } |
            Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
            Select-Object -First 1
    }
    if (-not $Python -or -not [System.IO.Path]::IsPathRooted($Python) -or
        -not (Test-Path -LiteralPath $Python -PathType Leaf)) {
        throw "History upgrade requires Python 3.10 or newer. Rerun the installer with -PythonExe <absolute-path>."
    }
    & $Python $Helper inspect $database
    if ($LASTEXITCODE -eq 0) { return $null }
    if ($LASTEXITCODE -ne 75) { throw "History upgrade preflight failed; installation was left unchanged." }
    return @{ Database = [string]$database; PythonExe = $Python }
}

function Invoke-HistoryUpgrade {
    param([object]$Pending, [string]$Leases, [scriptblock]$Activate)

    $gates = @()
    $readers = @()
    try {
        # CMD checks these both before and after publishing its lease. PowerShell
        # readers are fenced by exclusive handles held through pointer publication.
        foreach ($generation in Get-ChildItem -LiteralPath $Leases -Directory) {
            $gatePath = Join-Path $generation.FullName ".pruning"
            $gate = [System.IO.File]::Open($gatePath, 'OpenOrCreate', 'ReadWrite', 'None')
            $gates += @{ Handle = $gate; Path = $gatePath; Generation = $generation.FullName }
        }
        $announced = $false
        foreach ($gate in $gates) {
            $reader = $null
            while ($null -eq $reader) {
                try {
                    $reader = [System.IO.File]::Open((Join-Path $gate.Generation 'powershell.lock'), 'Open', 'ReadWrite', 'None')
                } catch [System.IO.IOException] {
                    if (($_.Exception.HResult -band 65535) -ne 32) { throw }
                    if (-not $announced) {
                        Write-Host "Upgrade pending. Close Codex sessions; this launch will retry."
                        $announced = $true
                    }
                    Start-Sleep -Seconds 1
                }
            }
            $readers += $reader
        }
        # Do not reuse the pruner's permissive process-error handling here.
        while ($true) {
            $active = $false
            foreach ($gate in $gates) {
                foreach ($lease in Get-ChildItem -LiteralPath $gate.Generation -Filter 'cmd.*.lease' -File) {
                    if ($lease.Name -notmatch '^cmd\.(\d+)\.(\d+)\.lease$') { throw "Unknown Codex launch lease; upgrade remains pending." }
                    try { $process = [System.Diagnostics.Process]::GetProcessById([int]$Matches[1]) }
                    catch [System.ArgumentException] { continue }
                    if ($process.StartTime.ToUniversalTime().Ticks -eq [long]$Matches[2]) { $active = $true }
                }
            }
            if (-not $active) { break }
            if (-not $announced) {
                Write-Host "Upgrade pending. Close Codex sessions; this launch will retry."
                $announced = $true
            }
            Start-Sleep -Seconds 1
        }
        $helper = Join-Path (Split-Path $Pending.Installer) 'windows_upgrade.py'
        & $Pending.PythonExe $helper complete $Pending.Database
        if ($LASTEXITCODE -ne 0) { throw "History upgrade remains pending. The current installation was not activated." }
        & $Activate
    } finally {
        foreach ($reader in $readers) { $reader.Dispose() }
        foreach ($gate in $gates) {
            $gate.Handle.Dispose()
            Remove-Item -LiteralPath $gate.Path -Force
        }
    }
}
