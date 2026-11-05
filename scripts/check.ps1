$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$projectRoot = Split-Path -Parent $PSScriptRoot
$desktopRoot = Join-Path $projectRoot "apps\desktop"

Push-Location $projectRoot
try {
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace --all-targets
} finally {
    Pop-Location
}

Push-Location $desktopRoot
try {
    npm ci
    npm run build
} finally {
    Pop-Location
}

