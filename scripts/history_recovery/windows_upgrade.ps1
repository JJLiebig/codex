# Only the managed Windows installer calls these functions, under its install locks.
function Get-HistoryUpgrade {
    param([string]$Target, [string]$Python, [string]$Helper, [string]$Database)

    # The new executable only resolves configuration; it must not open a database.
    if (-not $Database) {
        $Database = & $Target debug history-upgrade-target
        if ($LASTEXITCODE -ne 0 -or @($Database).Count -ne 1 -or
            -not [System.IO.Path]::IsPathRooted($Database)) {
            throw "Cannot resolve history for this upgrade. Installation was left unchanged."
        }
    }
    if (-not (Test-Path -LiteralPath $Database -PathType Leaf)) {
        return @{ Database = [string]$Database; PythonExe = $Python; Required = $true }
    }
    if ([string]::IsNullOrWhiteSpace($Python)) {
        $Python = $env:Path -split ';' |
            Where-Object { [System.IO.Path]::IsPathRooted($_) } |
            ForEach-Object { Join-Path $_ 'python.exe' } |
            Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
            Select-Object -First 1
    }
    if (-not $Python -or -not [System.IO.Path]::IsPathRooted($Python) -or
        -not (Test-Path -LiteralPath $Python -PathType Leaf)) {
        throw "History upgrade requires Python 3.10 or newer. Rerun the installer with -PythonExe <absolute-path> (also -ResumeUpgrade if pending)."
    }
    & $Python $Helper inspect $Database
    if ($LASTEXITCODE -notin @(0, 75)) { throw "History upgrade preflight failed; installation was left unchanged." }
    return @{ Database = [string]$Database; PythonExe = $Python; Required = ($LASTEXITCODE -eq 75) }
}

function Invoke-HistoryUpgrade {
    param([object]$Pending, [string]$Leases, [string]$Links, [string]$Releases, [string]$Incoming, [scriptblock]$Activate)

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
        $history = Get-HistoryUpgrade -Database $Pending.Database -Python $Pending.PythonExe -Helper $helper
        # A preloaded old wrapper can resume after these gates close. Retire only
        # owned junctions before repair so it must reread the active generation.
        foreach ($gate in $gates) {
            $name = Split-Path -Leaf $gate.Generation
            if ($name -eq $Incoming) { continue }
            $linkPath = Join-Path $Links $name
            $link = Get-Item -LiteralPath $linkPath -Force -ErrorAction SilentlyContinue
            if (-not $link) { continue } # Resuming an interrupted maintenance.
            $expected = Join-Path (Join-Path $Releases $name) 'bin'
            if ($name -notmatch '^\d{8}T\d{13}Z$' -or -not (Test-IsJunction $link) -or
                $link.FullName -ine [System.IO.Path]::GetFullPath($linkPath) -or
                (Resolve-PhysicalPath $link.FullName) -ine (Resolve-PhysicalPath $expected)) {
                throw "Unknown Codex generation; upgrade remains pending."
            }
            [System.IO.Directory]::Delete($link.FullName) # Unlink only; never recurse into the release.
        }
        if (Test-Path -LiteralPath $Pending.Database -PathType Leaf) {
            & $history.PythonExe $helper complete $Pending.Database
            if ($LASTEXITCODE -ne 0) { throw "History upgrade remains pending. The current installation was not activated." }
        }
        & $Activate
    } finally {
        foreach ($reader in $readers) { $reader.Dispose() }
        foreach ($gate in $gates) {
            $gate.Handle.Dispose()
            Remove-Item -LiteralPath $gate.Path -Force
        }
    }
}
