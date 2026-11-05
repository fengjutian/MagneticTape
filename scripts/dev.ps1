$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$projectRoot = Split-Path -Parent $PSScriptRoot
$desktopRoot = Join-Path $projectRoot "apps\desktop"

Push-Location $desktopRoot
try {
    if (-not (Test-Path "node_modules")) {
        npm install
    }
    npm run tauri dev
} finally {
    Pop-Location
}

