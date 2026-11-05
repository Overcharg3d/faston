# Builds faston (with the gui module) and faston-slim (without it) into
# target\release. `faston build` uses faston-slim for programs that don't
# import gui, which makes those executables much smaller.
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo build --release --no-default-features --target-dir target\slim
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Copy-Item target\slim\release\faston.exe target\release\faston-slim.exe -Force

foreach ($exe in 'faston.exe', 'faston-slim.exe') {
    '{0,-16} {1,6:N1} MB' -f $exe, ((Get-Item "target\release\$exe").Length / 1MB)
}
