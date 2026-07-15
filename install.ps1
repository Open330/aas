# aas installer for Windows — downloads the prebuilt binary from GitHub Releases.
#
#   irm https://raw.githubusercontent.com/open330/aas/main/install.ps1 | iex
#
# Env overrides:
#   $env:AAS_VERSION = "v0.1.8"           # pin a version (default: latest)
#   $env:AAS_BIN_DIR = "$HOME\bin"        # install location

$ErrorActionPreference = "Stop"
$repo = "open330/aas"
$bin = "aas"
$target = "x86_64-pc-windows-msvc"
$asset = "$bin-$target.zip"
$checksumAsset = "$bin-$target.sha256"

$version = if ($env:AAS_VERSION) { $env:AAS_VERSION } else { "latest" }
$url = if ($version -eq "latest") {
  "https://github.com/$repo/releases/latest/download/$asset"
} else {
  "https://github.com/$repo/releases/download/$version/$asset"
}
$checksumUrl = if ($version -eq "latest") {
  "https://github.com/$repo/releases/latest/download/$checksumAsset"
} else {
  "https://github.com/$repo/releases/download/$version/$checksumAsset"
}

$binDir = if ($env:AAS_BIN_DIR) { $env:AAS_BIN_DIR } else { "$env:LOCALAPPDATA\Programs\aas" }
New-Item -ItemType Directory -Force -Path $binDir | Out-Null

$tmp = New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP ("aas-" + [guid]::NewGuid()))
try {
  Write-Host "Downloading $asset ..."
  $zip = Join-Path $tmp $asset
  $checksum = Join-Path $tmp $checksumAsset
  Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
  Invoke-WebRequest -Uri $checksumUrl -OutFile $checksum -UseBasicParsing
  $expected = ((Get-Content -Raw $checksum).Trim() -split '\s+')[0].ToLowerInvariant()
  if ($expected -notmatch '^[0-9a-f]{64}$') { throw "invalid checksum file: $checksumAsset" }
  $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
  if ($actual -ne $expected) { throw "checksum verification failed for $asset" }

  Expand-Archive -Path $zip -DestinationPath $tmp -Force
  $exe = Get-ChildItem -Path $tmp -Recurse -Filter "$bin.exe" | Select-Object -First 1
  if (-not $exe) { throw "binary '$bin.exe' not found in archive" }
  $destination = Join-Path $binDir "$bin.exe"
  $stage = Join-Path $binDir (".$bin." + [guid]::NewGuid() + ".tmp.exe")
  Copy-Item $exe.FullName $stage
  & $stage --version
  if ($LASTEXITCODE -ne 0) { throw "downloaded binary failed its execution check" }
  if (Test-Path $destination) {
    $backup = Join-Path $binDir (".$bin." + [guid]::NewGuid() + ".backup.exe")
    [System.IO.File]::Replace($stage, $destination, $backup, $true)
    Remove-Item $backup -Force
  } else {
    [System.IO.File]::Move($stage, $destination)
  }
  Write-Host "Installed $bin -> $binDir\$bin.exe"

  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  if ($userPath -notlike "*$binDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$binDir", "User")
    Write-Host "Added $binDir to your user PATH (restart your shell)."
  }
  & $destination --version
  if ($LASTEXITCODE -ne 0) { throw "installed binary failed its execution check" }
} finally {
  if ($stage -and (Test-Path $stage)) { Remove-Item $stage -Force -ErrorAction SilentlyContinue }
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
