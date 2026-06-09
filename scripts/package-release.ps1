# Builds both binaries and packages them into release zip files under dist/.
#
# Usage:
#   pwsh scripts/package-release.ps1
#
# Requires:
#   - cargo on PATH
#   - For nsf-presenter: FFmpeg DLLs findable. Looks for $env:FFMPEG_DIR/bin
#     first; if unset, tries a few common locations.

$ErrorActionPreference = 'Stop'

# Resolve the repo root (one directory above this script's location).
$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $repoRoot

# Pull the version from the player crate's Cargo.toml.
$version = (Select-String -Path 'crates/nsf-player/Cargo.toml' `
                          -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
Write-Host "Packaging version $version"

$dist = Join-Path $repoRoot 'dist'
if (Test-Path $dist) {
    Remove-Item -Recurse -Force $dist
}
New-Item -ItemType Directory -Force -Path $dist | Out-Null

# --- Build -----------------------------------------------------------------

Write-Host "`n=== cargo build --release ==="
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$releaseDir   = Join-Path $repoRoot 'target/release'
$playerExe    = Join-Path $releaseDir 'nsf-player.exe'
$presenterExe = Join-Path $releaseDir 'nsf-presenter.exe'

foreach ($exe in @($playerExe, $presenterExe)) {
    if (-not (Test-Path $exe)) {
        throw "Expected built binary not found: $exe"
    }
}

# --- nsf-player (no FFmpeg) ------------------------------------------------

$playerName = "nsf-player-v$version-windows"
$playerPkg  = Join-Path $dist $playerName
New-Item -ItemType Directory -Force -Path $playerPkg | Out-Null
Copy-Item $playerExe $playerPkg
Copy-Item (Join-Path $repoRoot 'README.md') $playerPkg
Copy-Item (Join-Path $repoRoot 'LICENSE') $playerPkg
Compress-Archive -Path "$playerPkg/*" -DestinationPath "$dist/$playerName.zip" -Force
Write-Host "Wrote $dist/$playerName.zip"

# --- nsf-presenter (needs FFmpeg DLLs) -------------------------------------

# Find FFmpeg DLLs.
$ffmpegBin = $null
if ($env:FFMPEG_DIR) {
    $candidate = Join-Path $env:FFMPEG_DIR 'bin'
    if (Test-Path $candidate) { $ffmpegBin = $candidate }
}
if (-not $ffmpegBin) {
    foreach ($probe in @(
        'C:\ffmpeg\bin',
        'C:\Program Files\ffmpeg\bin',
        'C:\tools\ffmpeg\bin'
    )) {
        if (Test-Path $probe) { $ffmpegBin = $probe; break }
    }
}
if (-not $ffmpegBin) {
    Write-Warning "FFmpeg bin/ directory not found. Set `$env:FFMPEG_DIR or place FFmpeg DLLs alongside nsf-presenter.exe manually."
    Write-Warning "Skipping nsf-presenter packaging."
    return
}

Write-Host "Using FFmpeg DLLs from $ffmpegBin"

$presenterName = "nsf-presenter-v$version-windows"
$presenterPkg  = Join-Path $dist $presenterName
New-Item -ItemType Directory -Force -Path $presenterPkg | Out-Null
Copy-Item $presenterExe $presenterPkg
Copy-Item (Join-Path $repoRoot 'README.md') $presenterPkg
Copy-Item (Join-Path $repoRoot 'LICENSE') $presenterPkg

# Copy the major FFmpeg runtime DLLs. We grab any *-NN.dll matching the
# six libraries the renderer uses, plus any plain dependency *.dll the
# user happens to have (zlib1, etc.).
$ffmpegLibs = @('avcodec', 'avformat', 'avutil', 'swscale', 'swresample', 'avfilter', 'avdevice')
$copied = New-Object System.Collections.Generic.HashSet[string]
foreach ($lib in $ffmpegLibs) {
    Get-ChildItem -Path $ffmpegBin -Filter "$lib-*.dll" | ForEach-Object {
        Copy-Item $_.FullName $presenterPkg
        $copied.Add($_.Name) | Out-Null
    }
}
# Also grab common companions (zlib, etc.) — small, safer to include.
foreach ($name in @('zlib1.dll', 'libwinpthread-1.dll')) {
    $p = Join-Path $ffmpegBin $name
    if (Test-Path $p) {
        Copy-Item $p $presenterPkg
        $copied.Add($name) | Out-Null
    }
}
Write-Host "Bundled DLLs: $($copied -join ', ')"

Compress-Archive -Path "$presenterPkg/*" -DestinationPath "$dist/$presenterName.zip" -Force
Write-Host "Wrote $dist/$presenterName.zip"

Write-Host "`nDone. Artifacts in $dist :"
Get-ChildItem $dist -Filter '*.zip' | ForEach-Object {
    "{0,12:N0} bytes  {1}" -f $_.Length, $_.Name
}
