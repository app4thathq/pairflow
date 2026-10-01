# Embed packaging/windows/pairflow.exe.manifest into the release executables.
# Windows reads this manifest before any code runs, which is what makes the
# process per-monitor DPI aware. CI runs this after `cargo build --release`.
# The tray app is pairflow-gui.exe. pairflow.exe is the console CLI.
$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$manifest = Join-Path $PSScriptRoot "pairflow.exe.manifest"
if (-not (Test-Path $manifest)) { throw "missing $manifest" }
$exes = @(
    (Join-Path $root "target\release\pairflow-gui.exe"),
    (Join-Path $root "target\release\pairflow.exe")
) | Where-Object { Test-Path $_ }
if (-not $exes) { throw "missing target\release\pairflow-gui.exe" }
$mt = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Recurse -Filter mt.exe |
    Where-Object { $_.FullName -match '\\x64\\mt.exe$' } |
    Sort-Object FullName -Descending |
    Select-Object -First 1
if (-not $mt) { throw "mt.exe not found" }
foreach ($exe in $exes) {
    & $mt.FullName -nologo -manifest $manifest -outputresource:"${exe};#1"
    if ($LASTEXITCODE -ne 0) { throw "mt.exe failed on ${exe}: $LASTEXITCODE" }
    Write-Host "embedded per-monitor DPI manifest into $exe"
}
