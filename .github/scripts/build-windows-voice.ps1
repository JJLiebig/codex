param([Parameter(Mandatory = $true)][string]$Output)

$ErrorActionPreference = 'Stop'
# Resolve the runtime from this selected toolchain before downloading or building.
$runtimeDlls = @(Get-ChildItem -Path (Join-Path $env:VCToolsRedistDir 'x64/Microsoft.VC*.CRT/vcruntime140.dll') -File)
if ($runtimeDlls.Count -ne 1) { throw 'Expected one x64 Visual C++ runtime in VCToolsRedistDir' }
$redist = $runtimeDlls[0].DirectoryName

$Output = [IO.Path]::GetFullPath($Output)
New-Item -ItemType Directory -Path $Output -Force | Out-Null
$tools = Join-Path $Output 'tools'
New-Item -ItemType Directory -Path $tools -Force | Out-Null

function Get-VerifiedFile([string]$Url, [string]$Path, [string]$Sha256) {
    if (-not (Test-Path -LiteralPath $Path)) { Invoke-WebRequest $Url -OutFile $Path }
    if ((Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Sha256) {
        throw "Checksum mismatch: $Path"
    }
}

# Public, versioned installers; Cygwin verifies its signed package metadata.
$setup = Join-Path $tools 'setup.exe'
Get-VerifiedFile 'https://cygwin.com/setup/setup-2.939.x86_64.exe' $setup '73d9bf0be6b7adb8af8c2e709556159a45fa1892678f5105a619a1cdd056e51a'
$cygwin = Join-Path $tools 'cygwin'
$setupArgs = @('--quiet-mode', '--no-admin', '--no-desktop', '--no-shortcuts', '--no-startmenu',
    '--root', $cygwin, '--local-package-dir', (Join-Path $tools 'cache'), '--only-site',
    '--site', 'https://mirrors.kernel.org/sourceware/cygwin/',
    '--packages', 'make=4.4.1-2,automake1.18=1.18.1-2')
$setupProcess = Start-Process $setup -ArgumentList ($setupArgs | ForEach-Object { '"{0}"' -f $_ }) -WindowStyle Hidden -Wait -PassThru
if ($setupProcess.ExitCode -ne 0) { throw "Cygwin setup failed: $($setupProcess.ExitCode)" }

$msi = Join-Path $tools 'pkgconf.msi'
Get-VerifiedFile 'https://github.com/pkgconf/pkgconf/releases/download/pkgconf-3.0.7/pkgconf-x64-3.0.7.msi' $msi '7a316dba4a4498ea952b746c82deed41c597657095a0f776b75654191b45ae44'
$pkgconfRoot = Join-Path $tools 'pkgconf'
# Administrative extraction only; no machine-wide pkgconf installation.
$msiArgs = @('/a', "`"$msi`"", '/qn', "TARGETDIR=`"$pkgconfRoot`"")
$msiProcess = Start-Process msiexec.exe -ArgumentList $msiArgs -WindowStyle Hidden -Wait -PassThru
if ($msiProcess.ExitCode -ne 0) { throw "pkgconf extraction failed: $($msiProcess.ExitCode)" }
$pkgconf = Join-Path $pkgconfRoot 'PFiles64/pkgconf-3.0.7/pkgconf.exe'

$archives = Join-Path $Output 'archives'
New-Item -ItemType Directory -Path $archives -Force | Out-Null
foreach ($source in (Get-Content third_party/voice/sources.json -Raw | ConvertFrom-Json).sources) {
    Get-VerifiedFile $source.url (Join-Path $archives $source.archive) $source.sha256
}

$cc = (Get-Command cl.exe -ErrorAction Stop).Source
$nmake = (Get-Command nmake.exe -ErrorAction Stop).Source
$cmake = (Get-Command cmake.exe -ErrorAction Stop).Source
$commit = git rev-parse HEAD
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify source commit' }
python third_party/voice/build_native.py --archives $archives --output (Join-Path $Output 'native') `
    --target x86_64-pc-windows-msvc --cc $cc --cxx $cc --cmake $cmake `
    --make (Join-Path $cygwin 'bin/make.exe') --shell (Join-Path $cygwin 'bin/bash.exe') `
    --pkg-config $pkgconf --bootstrap-make $nmake --jobs 4
if ($LASTEXITCODE -ne 0) { throw 'Native voice build failed' }

python scripts/codex_package/codex_plus_plus/voice.py prepare --work $Output --commit $commit --redist $redist
if ($LASTEXITCODE -ne 0) { throw 'Voice runtime preparation failed' }

$env:PKG_CONFIG = $pkgconf
$env:PKG_CONFIG_LIBDIR = Join-Path $Output 'sdk/lib/pkgconfig'
$env:PKG_CONFIG_PATH = ''
$env:STABLE_GIT_COMMIT = $commit
$env:CARGO_BUILD_JOBS = '4'
Push-Location codex-rs
try {
    cargo build -p codex-voice-host --target x86_64-pc-windows-msvc --profile release -j 4
    if ($LASTEXITCODE -ne 0) { throw 'Voice helper build failed' }
    Copy-Item -LiteralPath target/x86_64-pc-windows-msvc/release/codex-voice-host.exe -Destination $Output
} finally {
    Pop-Location
}
