# Install the `brain` binary on Windows. No Rust toolchain, no build.
#
#   irm https://raw.githubusercontent.com/claudin-io/claudinio-brain/main/install.ps1 | iex
#
# Environment:
#   BRAIN_VERSION       release tag to install (default: newest stable, else `nightly`)
#   BRAIN_INSTALL_DIR   where to put it (default: %LOCALAPPDATA%\Programs\brain)

$ErrorActionPreference = 'Stop'

$repo = 'claudin-io/claudinio-brain'
$version = if ($env:BRAIN_VERSION) { $env:BRAIN_VERSION } else { '' }
$installDir = if ($env:BRAIN_INSTALL_DIR) { $env:BRAIN_INSTALL_DIR }
              else { Join-Path $env:LOCALAPPDATA 'Programs\brain' }

# Only x86_64 is published. An arm64 Windows machine runs it under emulation,
# which works but is worth saying out loud rather than silently shipping.
$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -eq 'ARM64') {
    Write-Host 'brain: no native arm64 build yet; installing the x64 build (runs emulated).'
} elseif ($arch -ne 'AMD64') {
    throw "unsupported architecture: $arch"
}
$asset = 'brain-x86_64-pc-windows-msvc.zip'

# /releases/latest ignores prereleases, so it returns nothing until the first
# `v*` tag exists. That is the signal to fall back to nightly, not an error.
if (-not $version) {
    try {
        $version = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
    } catch {
        $version = ''
    }
    if (-not $version) { $version = 'nightly' }
}

$base = "https://github.com/$repo/releases/download/$version"
Write-Host "brain: installing $version (x86_64-pc-windows-msvc)"

$tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP "brain-install-$PID")
try {
    $zip = Join-Path $tmp $asset
    Invoke-WebRequest "$base/$asset" -OutFile $zip -UseBasicParsing
    $sumsFile = Join-Path $tmp 'SHA256SUMS'
    Invoke-WebRequest "$base/SHA256SUMS" -OutFile $sumsFile -UseBasicParsing

    # Verify before anything reaches PATH.
    $want = (Get-Content $sumsFile |
        Where-Object { $_ -match "\s\*?$([regex]::Escape($asset))$" } |
        ForEach-Object { ($_ -split '\s+')[0] } | Select-Object -First 1)
    if (-not $want) { throw "$asset is not listed in SHA256SUMS" }
    $got = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    if ($got -ne $want.ToLower()) {
        throw "checksum mismatch for ${asset}:`n  expected $want`n  got      $got"
    }

    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    $exe = Join-Path $tmp 'brain.exe'
    if (-not (Test-Path $exe)) { throw "$asset did not contain brain.exe" }

    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    # Windows will not let you overwrite a running executable, so move the old
    # one aside instead of failing the upgrade. It disappears on the next reboot.
    $target = Join-Path $installDir 'brain.exe'
    if (Test-Path $target) {
        try { Remove-Item $target -Force }
        catch { Move-Item $target "$target.old-$PID" -Force }
    }
    Move-Item $exe $target -Force

    Write-Host "brain: installed to $target"
    & $target --version

    # PATH for the user, not the machine: no elevation, and it survives.
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($null -eq $userPath) { $userPath = '' }
    if (($userPath -split ';') -notcontains $installDir) {
        [Environment]::SetEnvironmentVariable(
            'Path', ($userPath.TrimEnd(';') + ';' + $installDir), 'User')
        Write-Host "brain: added $installDir to your PATH (open a new terminal for it to take effect)."
    }
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
