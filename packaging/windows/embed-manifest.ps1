# Embed packaging/windows/pairflow.exe.manifest into target/release/pairflow.exe.
# Windows reads this manifest before any code runs, which is what makes the
# process per-monitor DPI aware. CI runs this after `cargo build --release`.
$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$exe = Join-Path $root "target\release\pairflow.exe"
$manifest = Join-Path $PSScriptRoot "pairflow.exe.manifest"
if (-not (Test-Path $exe)) { throw "missing $exe" }
if (-not (Test-Path $manifest)) { throw "missing $manifest" }
$mt = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Recurse -Filter mt.exe |
    Where-Object { $_.FullName -match '\\x64\\mt.exe$' } |
    Sort-Object FullName -Descending |
    Select-Object -First 1
if (-not $mt) { throw "mt.exe not found" }
& $mt.FullName -nologo -manifest $manifest -outputresource:"${exe};#1"
if ($LASTEXITCODE -ne 0) { throw "mt.exe failed: $LASTEXITCODE" }
Write-Host "embedded per-monitor DPI manifest into $exe"
