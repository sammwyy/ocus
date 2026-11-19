param([string]$Packages = 'target/release', [string]$Version = '0.1.0')
$ErrorActionPreference = 'Stop'
$install = Join-Path $env:LOCALAPPDATA 'Ocus-installer-smoke'
if (Test-Path $install) { throw "Refusing to replace $install" }
$installers = @(Get-ChildItem $Packages -Filter '*setup.exe')
if ($installers.Count -ne 1) { throw 'Expected exactly one NSIS installer' }
$shortcut = Join-Path ([Environment]::GetFolderPath('StartMenu')) 'Programs/Ocus.lnk'
if (Test-Path $shortcut) { throw "Refusing to replace $shortcut" }
try {
    $process = Start-Process $installers[0].FullName -ArgumentList '/S', "/D=$install" -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Installer failed: $($process.ExitCode)" }
    $stdout = Join-Path $env:RUNNER_TEMP 'ocus-version.txt'
    $process = Start-Process "$install/ocus.exe" -ArgumentList '--version' -RedirectStandardOutput $stdout -Wait -PassThru
    $output = (Get-Content $stdout -Raw).Trim()
    Write-Output $output
    if ($process.ExitCode -ne 0 -or $output -notmatch "^ocus $([regex]::Escape($Version)) \([0-9a-f]{7}\)$") { throw 'Installed version smoke failed' }
    if (!(Test-Path $shortcut)) { throw 'Start Menu shortcut missing' }
    if (!(Get-ChildItem $install -Filter '*.dll')) { throw 'FFmpeg runtime DLLs missing' }
} finally {
    if (Test-Path "$install/uninstall.exe") {
        $process = Start-Process "$install/uninstall.exe" -ArgumentList '/S', "_?=$install" -Wait -PassThru
        if ($process.ExitCode -ne 0) { throw "Uninstaller failed: $($process.ExitCode)" }
    }
}
if ((Test-Path "$install/ocus.exe") -or (Test-Path $shortcut)) { throw 'Uninstall left application or shortcut behind' }
# NSIS cannot remove its running uninstaller when using _?= for synchronous execution.
Remove-Item $install -Recurse -Force
if (Test-Path $install) { throw 'Installation directory remains' }
Write-Output 'Windows NSIS install/shortcut/uninstall smoke test passed'
