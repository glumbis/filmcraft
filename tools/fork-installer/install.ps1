# Installs FilmCraft (with OpenFX plug-ins) for the current user.
# Run "Install FilmCraft.bat" from the unzipped download; nothing needs administrator rights.
#
#  1. Copies the app to %LOCALAPPDATA%\Programs\FilmCraft-OFX (closes nothing: asks you to quit
#     FilmCraft first if it's running).
#  2. Adds Start menu and desktop shortcuts.
#  3. Puts the small FC Test plug-in on OFX_PLUGIN_PATH, so OpenFX can be checked without a
#     commercial plug-in.
#
# -Source <folder>  install from this folder instead of the one the script is in (for example a
#                   local build folder containing filmcraft.exe and filmcraft-cli.exe).
# -Yes              answers every question with yes / never prompts (for scripted installs).

param(
    [string]$Source = $PSScriptRoot,
    [switch]$Yes
)

$ErrorActionPreference = 'Stop'
$dst = Join-Path $env:LOCALAPPDATA 'Programs\FilmCraft-OFX'

function Say($text, $color = 'Gray') { Write-Host $text -ForegroundColor $color }

Say ''
Say 'FilmCraft with OpenFX plug-ins' 'Cyan'

if (-not $Source -or -not (Test-Path -LiteralPath $Source -PathType Container)) {
    Say "The source folder does not exist: $Source" 'Red'
    exit 1
}
$src = (Resolve-Path -LiteralPath $Source).ProviderPath.TrimEnd('\')
Say "Installing from $src"
Say "Installing to   $dst"
Say ''

if (-not (Test-Path -LiteralPath (Join-Path $src 'filmcraft.exe'))) {
    Say 'filmcraft.exe is not in the source folder: unzip the whole download first, then run it again.' 'Red'
    exit 1
}
if ($src -ieq $dst.TrimEnd('\')) {
    Say 'The source folder is the install folder: run the installer from the unzipped download instead.' 'Red'
    exit 1
}

# 1. The app.
while (Get-Process filmcraft -ErrorAction SilentlyContinue) {
    if ($Yes) { Say 'FilmCraft is running: close it and run the installer again.' 'Red'; exit 1 }
    Read-Host 'FilmCraft is running. Close it, then press Enter to continue' | Out-Null
}
New-Item -ItemType Directory -Force $dst | Out-Null
Copy-Item (Join-Path $src 'filmcraft.exe') $dst -Force
$cli = Join-Path $src 'filmcraft-cli.exe'
if (Test-Path -LiteralPath $cli) {
    Copy-Item $cli $dst -Force
} else {
    Say '[--] filmcraft-cli.exe not found in the source folder: skipped' 'Yellow'
}
foreach ($f in 'README.md', 'FORK.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'NOTICE') {
    if (Test-Path -LiteralPath (Join-Path $src $f)) { Copy-Item (Join-Path $src $f) $dst -Force }
}
# The uninstaller comes from the source folder, or failing that from next to this script.
foreach ($f in 'uninstall.ps1', 'Uninstall FilmCraft.bat') {
    foreach ($dir in $src, $PSScriptRoot) {
        if ($dir -and (Test-Path -LiteralPath (Join-Path $dir $f))) {
            Copy-Item (Join-Path $dir $f) $dst -Force
            break
        }
    }
}
Say '[ok] App installed' 'Green'

# 2. Shortcuts.
$ws = New-Object -ComObject WScript.Shell
$links = @(
    (Join-Path ([Environment]::GetFolderPath('Programs')) 'FilmCraft (OFX).lnk'),
    (Join-Path ([Environment]::GetFolderPath('Desktop')) 'FilmCraft (OFX).lnk')
)
foreach ($l in $links) {
    $s = $ws.CreateShortcut($l)
    $s.TargetPath = Join-Path $dst 'filmcraft.exe'
    $s.WorkingDirectory = $dst
    $s.Description = 'FilmCraft with OpenFX plug-ins'
    $s.Save()
}
Say '[ok] Start menu and desktop shortcuts: "FilmCraft (OFX)"' 'Green'

# 3. The test plug-in, on OFX_PLUGIN_PATH (added to, never replaced).
$bundle = Join-Path $src 'fc-test-ofx.ofx.bundle'
if (Test-Path -LiteralPath $bundle) {
    $ofx = Join-Path $dst 'OFX'
    New-Item -ItemType Directory -Force $ofx | Out-Null
    Copy-Item $bundle $ofx -Recurse -Force
    $cur = [Environment]::GetEnvironmentVariable('OFX_PLUGIN_PATH', 'User')
    $parts = @(if ($cur) { $cur -split ';' | Where-Object { $_ } })
    if ($parts -notcontains $ofx) {
        [Environment]::SetEnvironmentVariable('OFX_PLUGIN_PATH', (($parts + $ofx) -join ';'), 'User')
    }
    Say '[ok] Test plug-in installed ("FC Test" effects)' 'Green'
}
Say '     Your own OpenFX plug-ins (RSMB, Twixtor, Sapphire...): install their OFX versions with their own installers.'

Say ''
Say 'Done. Start FilmCraft from the "FilmCraft (OFX)" shortcut.' 'Cyan'
Say 'Windows may warn the first time ("Windows protected your PC"): More info > Run anyway (the app is not code-signed).'
