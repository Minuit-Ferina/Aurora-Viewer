<#
.SYNOPSIS
    Puts the built viewer and its files in a folder ready to launch (or to
    zip): exe, assets, licences. The GitHub release and build-release.ps1
    both use it: their folders are identical. The Linux job of the release
    runs it too (PowerShell 7), with the executable without extension.

.EXAMPLE
    ./scripts/package.ps1 -Exe target/release/aurora-viewer.exe -Out Aurora-Viewer
.EXAMPLE
    ./scripts/package.ps1 -Exe target/release/aurora-viewer -Out Aurora-Viewer-v0.8.0-linux-x64
#>
param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$Out,
    [ValidateSet('classic','ftrlv')][string]$Edition = 'classic',
    # repository whose assets go with the exe (default: this script's)
    [string]$Source = (Split-Path $PSScriptRoot -Parent)
)

$taskExePath = (Resolve-Path -LiteralPath $Exe -ErrorAction Stop).Path
$taskEdition = (& $taskExePath --edition | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $taskEdition -cne $Edition) {
    throw "Package expects edition '$Edition'; executable reported '$taskEdition'."
}
$taskVersion = (& $taskExePath --language en --version | Out-String).Trim() -replace '^Aurora Viewer ', ''
if ($LASTEXITCODE -ne 0 -or $taskVersion -notmatch '^\d+\.\d+\.\d+([+-][0-9A-Za-z.+-]+)?$') { throw 'Invalid package version.' }
$taskJson = @{ schema = 2; version = $taskVersion; edition = $Edition } | ConvertTo-Json

New-Item -ItemType Directory -Force -Path (Join-Path $Out 'assets/emoji') -ErrorAction Stop | Out-Null
Copy-Item -LiteralPath $Exe -Destination $Out -ErrorAction Stop
# The emoji font is inside the exe; its licence ships, and the assets folder
# stays: the updater of the published versions refuses a package without it.
Copy-Item -LiteralPath (Join-Path $Source 'assets/emoji/LICENSE-OFL.txt') -Destination (Join-Path $Out 'assets/emoji') -ErrorAction Stop
# The spelling dictionaries are inside the exe too: their licences ship
# (GPL, LGPL and MPL ask for the text), one per dictionary, with the list of
# their sources.
$dictionaries = Join-Path $Source 'crates/aurora-viewer/assets/dictionaries'
$dictionariesOut = Join-Path $Out 'assets/dictionaries'
New-Item -ItemType Directory -Force -Path $dictionariesOut -ErrorAction Stop | Out-Null
Copy-Item -LiteralPath (Join-Path $dictionaries 'README.md') -Destination $dictionariesOut -ErrorAction Stop
Get-ChildItem -LiteralPath $dictionaries -Directory -ErrorAction Stop | ForEach-Object {
    $license = Join-Path $_.FullName 'LICENSE.txt'
    if (Test-Path -LiteralPath $license) {
        Copy-Item -LiteralPath $license -Destination (Join-Path $dictionariesOut "$($_.Name)-LICENSE.txt") -ErrorAction Stop
    }
}
$docs = 'LICENSE', 'NOTICE.md', 'README.md' | ForEach-Object { Join-Path $Source $_ }
Copy-Item -LiteralPath $docs -Destination $Out -ErrorAction Stop
# Linux (an executable without extension): the desktop entry and its icon,
# named after the viewer's Wayland app_id / X11 WM_CLASS (cli.rs DESKTOP_ID)
# so the desktop matches the window to them. docs/LINUX.md installs them.
# (macOS has its own bundle: package-macos.ps1; $IsMacOS is unset, so
# false, in Windows PowerShell 5.1.)
if (-not [IO.Path]::GetExtension($Exe) -and -not $IsMacOS) {
    $desktop = Get-Content -LiteralPath (Join-Path $Source 'packaging/linux/aurora-viewer.desktop') -Raw -ErrorAction Stop
    $desktopId = if ($Edition -eq 'ftrlv') { 'aurora-viewer-ftrlv' } else { 'aurora-viewer' }
    if ($Edition -eq 'ftrlv') {
        $desktop = $desktop.Replace('Name=Aurora Viewer', 'Name=Aurora Viewer FTRLV').Replace('StartupWMClass=aurora-viewer', "StartupWMClass=$desktopId")
        $desktop = $desktop -replace '(?m)^Exec=aurora-viewer(?=\s|$)', "Exec=$desktopId"
    }
    if ($Edition -eq 'ftrlv') {
        Move-Item -LiteralPath (Join-Path $Out (Split-Path $Exe -Leaf)) -Destination (Join-Path $Out $desktopId) -ErrorAction Stop
    }
    [IO.File]::WriteAllText((Join-Path (Resolve-Path -LiteralPath $Out).Path "$desktopId.desktop"), $desktop, [Text.UTF8Encoding]::new($false))
    Copy-Item -LiteralPath (Join-Path $Source 'assets/branding/aurora-logo.svg') -Destination (Join-Path $Out 'aurora-viewer.svg') -ErrorAction Stop
}
$taskMetadataPath = Join-Path (Resolve-Path -LiteralPath $Out -ErrorAction Stop).Path 'aurora-install.json'
# Windows PowerShell 5.1 does not support Set-Content -Encoding utf8NoBOM.
[IO.File]::WriteAllText($taskMetadataPath, $taskJson, [Text.UTF8Encoding]::new($false))
