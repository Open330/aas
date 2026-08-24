# aas installer for Windows — downloads the prebuilt binary from GitHub Releases.
#
#   irm https://raw.githubusercontent.com/open330/aas/main/install.ps1 | iex
#
# Env overrides:
#   $env:AAS_VERSION = "v0.1.9"           # pin a version (default: latest)
#   $env:AAS_BIN_DIR = "$HOME\bin"        # install location
#   $env:AAS_SKIP_ATTESTATION = "1"         # skip provenance verification (not recommended)
#   $env:AAS_DOWNLOAD_BASE = "https://..."  # release-asset base URL (testing/mirror use)

$ErrorActionPreference = "Stop"
$repo = "open330/aas"
$signerWorkflow = "Open330/aas/.github/workflows/release.yml"
$bin = "aas"
$target = "x86_64-pc-windows-msvc"
$asset = "$bin-$target.zip"
$checksumAsset = "$bin-$target.sha256"

$version = if ($env:AAS_VERSION) { $env:AAS_VERSION } else { "latest" }
$url = if ($env:AAS_DOWNLOAD_BASE) {
  $env:AAS_DOWNLOAD_BASE.TrimEnd('/') + "/$asset"
} elseif ($version -eq "latest") {
  "https://github.com/$repo/releases/latest/download/$asset"
} else {
  "https://github.com/$repo/releases/download/$version/$asset"
}
$checksumUrl = if ($env:AAS_DOWNLOAD_BASE) {
  $env:AAS_DOWNLOAD_BASE.TrimEnd('/') + "/$checksumAsset"
} elseif ($version -eq "latest") {
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

  if ($env:AAS_SKIP_ATTESTATION -ne "1") {
    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
      throw "GitHub CLI (gh) is required for release attestation verification; install gh or explicitly set AAS_SKIP_ATTESTATION=1"
    }
    & gh attestation verify $zip `
      --repo $repo `
      --signer-workflow $signerWorkflow `
      --deny-self-hosted-runners | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "release attestation verification failed" }
  }

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
  $separator = [IO.Path]::PathSeparator
  $normalizedBinDir = [IO.Path]::GetFullPath($binDir).TrimEnd(
    [IO.Path]::DirectorySeparatorChar,
    [IO.Path]::AltDirectorySeparatorChar
  )
  $pathEntries = @($userPath -split [Regex]::Escape([string]$separator) | Where-Object { $_ })
  $alreadyPresent = $false
  foreach ($entry in $pathEntries) {
    try {
      $normalizedEntry = [IO.Path]::GetFullPath($entry).TrimEnd(
        [IO.Path]::DirectorySeparatorChar,
        [IO.Path]::AltDirectorySeparatorChar
      )
      if ([string]::Equals($normalizedEntry, $normalizedBinDir, [StringComparison]::OrdinalIgnoreCase)) {
        $alreadyPresent = $true
        break
      }
    } catch {
      # Preserve malformed legacy entries, but never treat a substring/wildcard match as equal.
    }
  }
  if (-not $alreadyPresent) {
    $newUserPath = (@($pathEntries) + $binDir) -join $separator
    [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
    Write-Host "Added $binDir to your user PATH (restart your shell)."
  }
  & $destination --version
  if ($LASTEXITCODE -ne 0) { throw "installed binary failed its execution check" }
} finally {
  if ($stage -and (Test-Path $stage)) { Remove-Item $stage -Force -ErrorAction SilentlyContinue }
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
