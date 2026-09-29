# Installs the auto-ascii command on Windows from a GitHub release:
#
#   powershell -c "irm https://github.com/andmckay01/auto-ascii/releases/latest/download/install.ps1 | iex"
#
# $env:AUTO_ASCII_VERSION = '0.3.0' (or -Version) installs that release instead
# of the latest; $env:AUTO_ASCII_INSTALL_DIR (or -InstallDir) overrides
# %LOCALAPPDATA%\Programs\auto-ascii. The download is checked against its
# published SHA-256. No admin rights needed.
# Errors throw rather than exit, so a failure under `iex` leaves the caller's
# shell open.
param(
    [string]$Version = $(if ($env:AUTO_ASCII_VERSION) { $env:AUTO_ASCII_VERSION } else { 'latest' }),
    [string]$InstallDir = $(if ($env:AUTO_ASCII_INSTALL_DIR) { $env:AUTO_ASCII_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\auto-ascii' })
)

$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1: without these, GitHub's TLS 1.2 is refused on older
# systems and the progress bar slows Invoke-WebRequest to a crawl.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$ProgressPreference = 'SilentlyContinue'

$Repo = 'andmckay01/auto-ascii'

# A 32-bit PowerShell on 64-bit Windows reports x86 here and the real
# architecture in PROCESSOR_ARCHITEW6432.
$arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
$target = switch ($arch) {
    'AMD64' { 'x86_64-pc-windows-msvc' }
    'ARM64' { 'aarch64-pc-windows-msvc' }
    default { throw "auto-ascii install: unsupported architecture $arch (try: cargo install auto-ascii)" }
}

$base = if ($Version -eq 'latest') {
    "https://github.com/$Repo/releases/latest/download"
} else {
    "https://github.com/$Repo/releases/download/v$($Version -replace '^v', '')"
}
$archive = "auto-ascii-$target.zip"

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("auto-ascii-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $zipPath = Join-Path $tmp $archive
    $sumPath = "$zipPath.sha256"
    Write-Host "downloading $archive ($Version)"
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$archive" -OutFile $zipPath
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$archive.sha256" -OutFile $sumPath
    } catch {
        throw "auto-ascii install: could not download $base/$archive (is $Version a published release?): $_"
    }

    $expected = ((Get-Content -Raw $sumPath).Trim() -split '\s+')[0]
    $actual = (Get-FileHash -Algorithm SHA256 -Path $zipPath).Hash
    if ($actual -ne $expected) {
        throw "auto-ascii install: SHA-256 mismatch for $archive (expected $expected, got $actual); refusing to install"
    }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Expand-Archive -Path $zipPath -DestinationPath $InstallDir -Force
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

$exe = Join-Path $InstallDir 'auto-ascii.exe'
$installed = & $exe --version
if ($LASTEXITCODE -ne 0) {
    throw "auto-ascii install: copied to $exe, but the downloaded binary failed to run (exit $LASTEXITCODE)"
}
Write-Host "installed $installed to $exe"

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = @(if ($userPath) { $userPath -split ';' | Where-Object { $_ } })
if (($entries | ForEach-Object { $_.TrimEnd('\') }) -notcontains $InstallDir.TrimEnd('\')) {
    [Environment]::SetEnvironmentVariable('Path', (($entries + $InstallDir) -join ';'), 'User')
    $env:Path = "$InstallDir;$env:Path"
    Write-Host "added $InstallDir to your user PATH; this window has it now, other open terminals need a restart"
}

Write-Host @'

Get started:

    auto-ascii add "https://youtu.be/jNQXAC9IVRw"   # import a YouTube link into your library
    auto-ascii play me-at-the-zoo                   # q quits
    auto-ascii stream me at the zoo                 # play the first search result live, saving nothing
'@
