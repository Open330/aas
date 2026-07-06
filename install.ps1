# aas installer for Windows — downloads the prebuilt binary from GitHub Releases.
#
#   irm https://raw.githubusercontent.com/open330/aas/main/install.ps1 | iex
#
# Env overrides:
#   $env:AAS_VERSION = "v0.1.0"           # pin a version (default: latest)
#   $env:AAS_BIN_DIR = "$HOME\bin"        # install location

$ErrorActionPreference = "Stop"
$repo = "open330/aas"
$bin = "aas"
$target = "x86_64-pc-windows-msvc"
$asset = "$bin-$target.zip"

$version = if ($env:AAS_VERSION) { $env:AAS_VERSION } else { "latest" }
$url = if ($version -eq "latest") {
  "https://github.com/$repo/releases/latest/download/$asset"
} else {
  "https://github.com/$repo/releases/download/$version/$asset"
}

$binDir = if ($env:AAS_BIN_DIR) { $env:AAS_BIN_DIR } else { "$env:LOCALAPPDATA\Programs\aas" }
New-Item -ItemType Directory -Force -Path $binDir | Out-Null

$tmp = New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP ("aas-" + [guid]::NewGuid()))
try {
  Write-Host "Downloading $asset ..."
  $zip = Join-Path $tmp $asset
  Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
  Expand-Archive -Path $zip -DestinationPath $tmp -Force
  $exe = Get-ChildItem -Path $tmp -Recurse -Filter "$bin.exe" | Select-Object -First 1
  if (-not $exe) { throw "binary '$bin.exe' not found in archive" }
  Copy-Item $exe.FullName (Join-Path $binDir "$bin.exe") -Force
  Write-Host "Installed $bin -> $binDir\$bin.exe"

  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  if ($userPath -notlike "*$binDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$binDir", "User")
    Write-Host "Added $binDir to your user PATH (restart your shell)."
  }
  & (Join-Path $binDir "$bin.exe") --version
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
