<#
.SYNOPSIS
    Builds one compiled edition and writes a complete portable archive and SHA-256.
    Run in a task worktree. Default profile is dev for previews. Use -Release
    for distribution. Each invocation stages its own edition before returning.
.EXAMPLE
    ./scripts/build-edition.ps1 -Edition ftrlv -Out .scratch/dist
#>
param(
    [Parameter(Mandatory)][ValidateSet('classic','ftrlv')][string]$Edition,
    [Parameter(Mandatory)][string]$Out,
    [switch]$Release
)
. "$PSScriptRoot\common.ps1"

$root = Split-Path $PSScriptRoot -Parent
$destination = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Out)
$windows = $PSVersionTable.PSEdition -eq 'Desktop' -or $IsWindows
$platform = if ($windows) { 'windows' } elseif ($IsMacOS) { 'macos' } else { 'linux' }
$arch = switch ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()) {
    'X64' { 'x64' }
    'Arm64' { 'arm64' }
    default { throw 'Unsupported platform architecture.' }
}
$taskBuildFolder = if ($Release) { 'release' } else { 'debug' }
$features = @()
if ($Edition -eq 'ftrlv') { $features += 'rlva-always-on' }
if ($Release -and $windows) { $features += 'xr-static-loader' }
$buildArgs = @('build','--locked','-p','aurora-viewer')
if ($Release) { $buildArgs += '--release' }
if ($features.Count) { $buildArgs += @('--features', ($features -join ',')) }

Push-Location -LiteralPath $root
$slot = $null
try {
    Sync-BuildCache
    $slot = Enter-BuildSlot
    Invoke-Checked 'cargo build' { cargo @buildArgs }
    $exe = Join-Path $root "target/$taskBuildFolder/aurora-viewer$(if ($windows) { '.exe' })"
    $version = (& $exe --language en --version | Out-String).Trim() -replace '^Aurora Viewer ', ''
    if ($LASTEXITCODE -ne 0 -or $version -notmatch '^\d+\.\d+\.\d+([+-][0-9A-Za-z.+-]+)?$') {
        throw 'Invalid compiled version.'
    }
    $label = if ($Edition -eq 'ftrlv') { 'FTRLV' } else { 'Classic' }
    $name = "Aurora-Viewer-$label-v$version-$platform-$arch"
    if (!$Release) { $name += '-dev' }
    $format = if ($windows -or $IsMacOS) { '.zip' } else { '.tar.gz' }
    $archive = Join-Path $destination "$name$format"
    if ((Test-Path -LiteralPath $archive) -or (Test-Path -LiteralPath "$archive.sha256")) {
        throw "Archive or checksum already exists: $archive"
    }
    New-Item -ItemType Directory -Force -Path $destination -ErrorAction Stop | Out-Null
    $stage = Join-Path $destination ('.staging-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $stage -ErrorAction Stop | Out-Null
    if ($IsMacOS) {
        $bundle = "Aurora Viewer $label.app"
        & "$PSScriptRoot/package-macos.ps1" -Exe $exe -Out (Join-Path $stage $bundle) -Edition $Edition
        if ($Release) {
            Invoke-Checked 'Ad hoc code signing' { codesign --force --sign - (Join-Path $stage $bundle) }
        }
        Invoke-Checked 'macOS archive' { ditto -c -k --keepParent (Join-Path $stage $bundle) $archive }
    } else {
        $package = Join-Path $stage $name
        & "$PSScriptRoot/package.ps1" -Exe $exe -Out $package -Edition $Edition
        if ($windows) {
            Compress-Archive -LiteralPath $package -DestinationPath $archive -ErrorAction Stop
        } else {
            Invoke-Checked 'Linux archive' { tar -C $stage -czf $archive $name }
        }
    }
    $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256 -ErrorAction Stop).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText("$archive.sha256", "$hash  $([IO.Path]::GetFileName($archive))`n", [Text.UTF8Encoding]::new($false))
    Write-Host "Archive: $archive"
    Write-Host "Staged application: $stage"
} finally {
    Exit-BuildSlot $slot
    Pop-Location
}