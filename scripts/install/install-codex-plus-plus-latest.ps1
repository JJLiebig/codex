param(
    [string]$ShimDir = (Join-Path $env:LOCALAPPDATA "Programs\CodexPlusPlus\bin"),
    [switch]$AddToUserPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$architecture = if ([string]::IsNullOrWhiteSpace($env:PROCESSOR_ARCHITEW6432)) {
    $env:PROCESSOR_ARCHITECTURE
} else {
    $env:PROCESSOR_ARCHITEW6432
}
if ($env:OS -ne "Windows_NT" -or $architecture -notmatch "^(AMD64|x86_64)$") {
    throw "Unsupported Codex++ install target: $($env:OS) $architecture"
}

function Get-FinalUri {
    param([object]$Response)

    $baseResponse = $Response.BaseResponse
    if ($null -ne $baseResponse.PSObject.Properties["ResponseUri"]) {
        return [Uri]$baseResponse.ResponseUri
    }
    if ($null -ne $baseResponse.PSObject.Properties["RequestMessage"]) {
        return [Uri]$baseResponse.RequestMessage.RequestUri
    }
    throw "Could not determine the resolved Codex++ release URL."
}

function Save-Asset {
    param(
        [string]$Name,
        [string]$Destination
    )

    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$downloadBase/$Name" -OutFile $Destination
        return $true
    } catch {
        return $false
    }
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

function Confirm-AssetHash {
    param(
        [string]$Name,
        [string]$Path
    )

    $checksum = [System.IO.File]::ReadAllText("$Path.sha256")
    $pattern = "\A([0-9a-fA-F]{64})[ \t]+$([regex]::Escape($Name))(?:\r?\n)?\z"
    $match = [regex]::Match($checksum, $pattern)
    if (-not $match.Success) {
        throw "Malformed SHA-256 sidecar for Codex++ release asset: $Name"
    }
    $actual = Get-Sha256 -Path $Path
    if ($actual -ine $match.Groups[1].Value) {
        throw "SHA-256 mismatch for Codex++ release asset: $Name"
    }
}

$releaseBase = if ([string]::IsNullOrWhiteSpace($env:CODEX_PLUS_PLUS_RELEASE_BASE_URL)) {
    "https://github.com/JJLiebig/codex"
} else {
    $env:CODEX_PLUS_PLUS_RELEASE_BASE_URL.TrimEnd("/")
}

function Get-VersionKey([string]$Version) {
    if ($Version -match '^(\d+)\.(\d+)\.(\d+)-fork\.(\d+)$') {
        return [Version]::new($Matches[1], $Matches[2], $Matches[3], $Matches[4])
    }
    return $null
}

$registry = if ($env:CODEX_PLUS_PLUS_NPM_REGISTRY_URL) {
    $env:CODEX_PLUS_PLUS_NPM_REGISTRY_URL.TrimEnd('/')
} else { 'https://registry.npmjs.org/@jjliebig%2fcodex-plus-plus' }
$npm = $null
$npmVersion = $null
try {
    $npm = Invoke-RestMethod -Uri "$registry/win32-x64" -TimeoutSec 10
    if ($npm.version -match '^(.+)-win32-x64$' -and (Get-VersionKey $Matches[1])) {
        $npmVersion = $npm.version -replace '-win32-x64$', ''
    }
} catch { $npm = $null }
$githubVersion = $null
try {
    $response = Invoke-WebRequest -UseBasicParsing -Uri "$releaseBase/releases/latest" -TimeoutSec 10
    $tag = (Get-FinalUri $response).Segments[-1].TrimEnd('/')
    if ($tag.StartsWith('codex-plus-plus-v')) {
        $candidate = $tag.Substring('codex-plus-plus-v'.Length)
        if (Get-VersionKey $candidate) { $githubVersion = $candidate }
    }
} catch { }
$current = Get-VersionKey $env:CODEX_PLUS_PLUS_CURRENT_VERSION
if ($current) {
    if ($npmVersion -and (Get-VersionKey $npmVersion) -lt $current) { $npmVersion = $null }
    if ($githubVersion -and (Get-VersionKey $githubVersion) -lt $current) { $githubVersion = $null }
}

$installerName = 'install-codex-plus-plus.ps1'
$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) "codex-plus-plus.$PID.$([Guid]::NewGuid())"
New-Item -ItemType Directory -Path $tempDir | Out-Null

function Get-GithubPackage {
    if (-not $githubVersion) { return $false }
    $archiveName = "codex-plus-plus-$githubVersion-x86_64-pc-windows-msvc.zip"
    $downloadBase = "$releaseBase/releases/download/codex-plus-plus-v$githubVersion"
    $archivePath = Join-Path $tempDir $archiveName
    if (-not (Save-Asset $archiveName $archivePath) -or
        -not (Save-Asset "$archiveName.sha256" "$archivePath.sha256")) { return $false }
    Confirm-AssetHash $archiveName $archivePath
    $script:packageDir = Join-Path $tempDir 'github'
    Expand-Archive -LiteralPath $archivePath -DestinationPath $packageDir
    $script:installerPath = Join-Path $packageDir "codex-resources/install/$installerName"
    if (-not (Test-Path -LiteralPath $installerPath)) {
        $script:installerPath = Join-Path $tempDir $installerName
        if (-not (Save-Asset $installerName $installerPath) -or
            -not (Save-Asset "$installerName.sha256" "$installerPath.sha256")) { return $false }
        Confirm-AssetHash $installerName $installerPath
    }
    return (Test-Path "$packageDir/bin/codex.exe") -and (Test-Path "$packageDir/codex-package.json")
}

function Get-NpmPackage {
    if (-not $npmVersion -or -not $npm.PSObject.Properties['dist'] -or $null -eq $npm.dist) { return $false }
    if (-not $npm.dist.PSObject.Properties['tarball'] -or -not $npm.dist.tarball) { return $false }
    if (-not $npm.dist.PSObject.Properties['integrity'] -or
        $npm.dist.integrity -cnotmatch '^sha512-[A-Za-z0-9+/]{86}==$') {
        throw 'Invalid npm SHA-512 integrity.'
    }
    $archivePath = Join-Path $tempDir 'npm.tgz'
    try { Invoke-WebRequest -UseBasicParsing -Uri $npm.dist.tarball -OutFile $archivePath }
    catch { return $false }
    $stream = [IO.File]::OpenRead($archivePath)
    $sha512 = [Security.Cryptography.SHA512]::Create()
    try { $actual = 'sha512-' + [Convert]::ToBase64String($sha512.ComputeHash($stream)) }
    finally { $sha512.Dispose(); $stream.Dispose() }
    if ($actual -cne $npm.dist.integrity) { throw 'npm SHA-512 mismatch.' }
    $extractDir = Join-Path $tempDir 'npm'
    New-Item -ItemType Directory $extractDir | Out-Null
    & tar.exe -xzf $archivePath -C $extractDir
    if ($LASTEXITCODE -ne 0) { throw 'Could not extract verified npm package.' }
    $script:packageDir = Join-Path $extractDir 'package/vendor/x86_64-pc-windows-msvc'
    if (-not (Test-Path "$packageDir/bin/codex.exe") -or
        -not (Test-Path "$packageDir/codex-package.json")) { return $false }
    $script:installerPath = Join-Path $packageDir "codex-resources/install/$installerName"
    if (-not (Test-Path -LiteralPath $installerPath)) { $script:installerPath = $env:CODEX_PLUS_PLUS_INSTALLER_PATH }
    return $installerPath -and (Test-Path -LiteralPath $installerPath -PathType Leaf)
}

try {
    if ($githubVersion -and (-not $npmVersion -or
        (Get-VersionKey $githubVersion) -gt (Get-VersionKey $npmVersion))) {
        $ready = (Get-GithubPackage) -or (Get-NpmPackage)
    } else { $ready = (Get-NpmPackage) -or (Get-GithubPackage) }
    if (-not $ready) { throw 'No usable published Codex++ package.' }
    $installerArgs = @{ TargetExe = "$packageDir/bin/codex.exe"; ShimDir = $ShimDir; Install = $true }
    if ($AddToUserPath) { $installerArgs.Add('AddToUserPath', $true) }
    $global:LASTEXITCODE = 0
    & $installerPath @installerArgs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
