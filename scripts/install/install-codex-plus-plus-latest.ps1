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
    } catch {
        throw "Could not download required Codex++ release asset '$Name': $($_.Exception.Message)"
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
try {
    $releaseResponse = Invoke-WebRequest -UseBasicParsing -Uri "$releaseBase/releases/latest"
} catch {
    throw "Could not resolve the latest stable Codex++ release: $($_.Exception.Message)"
}
$releaseUri = Get-FinalUri -Response $releaseResponse
$tag = $releaseUri.Segments[-1].TrimEnd("/")
if ($tag -notmatch "^codex-plus-plus-v(?<version>[0-9]+\.[0-9]+\.[0-9]+-fork\.[0-9]+)$") {
    throw "Latest release is not a stable Codex++ release: $tag"
}

$archiveName = "codex-plus-plus-$($Matches.version)-x86_64-pc-windows-msvc.zip"
$installerName = "install-codex-plus-plus.ps1"
$downloadBase = "$releaseBase/releases/download/$tag"
$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) "codex-plus-plus.$PID.$([Guid]::NewGuid())"
New-Item -ItemType Directory -Path $tempDir | Out-Null
try {
    $installerPath = Join-Path $tempDir $installerName
    Save-Asset -Name $installerName -Destination $installerPath
    Save-Asset -Name "$installerName.sha256" -Destination "$installerPath.sha256"
    Confirm-AssetHash -Name $installerName -Path $installerPath

    $archivePath = Join-Path $tempDir $archiveName
    Save-Asset -Name $archiveName -Destination $archivePath
    Save-Asset -Name "$archiveName.sha256" -Destination "$archivePath.sha256"
    Confirm-AssetHash -Name $archiveName -Path $archivePath

    $packageDir = Join-Path $tempDir "package"
    Expand-Archive -LiteralPath $archivePath -DestinationPath $packageDir
    $targetExe = Join-Path $packageDir "bin\codex.exe"
    if (-not (Test-Path -LiteralPath $targetExe -PathType Leaf)) {
        throw "Verified Codex++ archive does not contain bin\codex.exe."
    }

    $installerArgs = @{
        TargetExe = $targetExe
        ShimDir = $ShimDir
        Install = $true
    }
    if ($AddToUserPath) {
        $installerArgs.Add("AddToUserPath", $true)
    }
    & $installerPath @installerArgs
} finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
