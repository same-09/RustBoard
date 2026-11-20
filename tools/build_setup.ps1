# Builds the custom setup:  dist-setup/Rustboard_<version>_x64-setup.exe
# Run from the repo root (needs Rust + `cargo install tauri-cli --version "^2" --locked`).
#   pwsh tools/build_setup.ps1
$ErrorActionPreference = "Stop"
function Check($what) { if ($LASTEXITCODE -ne 0) { throw "$what failed (exit $LASTEXITCODE)" } }

$version = (Get-Content src-tauri/tauri.conf.json -Raw | ConvertFrom-Json).version

# 1. the app itself, exe only (no NSIS bundle)
Push-Location src-tauri
cargo tauri build --no-bundle; Check "app build"
Pop-Location

# 2. UI pages + the app exe go into the setup project
New-Item -ItemType Directory -Force setup/dist, setup/src-tauri/payload, dist-setup | Out-Null
Copy-Item installer-ui/index.html      setup/dist/index.html      -Force
Copy-Item uninstaller-ui/index.html    setup/dist/uninstall.html  -Force
Copy-Item src-tauri/target/release/rustboard.exe setup/src-tauri/payload/rustboard.exe -Force

# 3. the setup app (embeds rustboard.exe)
Push-Location setup/src-tauri
cargo tauri build --no-bundle; Check "setup build"
Pop-Location

$out = "dist-setup/Rustboard_${version}_x64-setup.exe"
Copy-Item setup/src-tauri/target/release/rustboard-setup.exe $out -Force
Write-Host "OK: $out"
