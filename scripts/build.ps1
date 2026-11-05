# Builds faston and the two runtimes `faston build` puts programs on, into
# target\release:
#   faston.exe         the full toolchain (code generator, gui)
#   faston-rt.exe      runtime for programs that don't import gui
#   faston-rt-gui.exe  runtime for programs that do
# The runtimes have no code generator: `faston build` compiles the program to
# machine code and saves it in the executable, so built programs stay small.
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo build --release --no-default-features --target-dir target\rt
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo build --release --no-default-features --features gui --target-dir target\rt-gui
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Copy-Item target\rt\release\faston.exe target\release\faston-rt.exe -Force
Copy-Item target\rt-gui\release\faston.exe target\release\faston-rt-gui.exe -Force

foreach ($exe in 'faston.exe', 'faston-rt.exe', 'faston-rt-gui.exe') {
    '{0,-18} {1,6:N1} MB' -f $exe, ((Get-Item "target\release\$exe").Length / 1MB)
}
