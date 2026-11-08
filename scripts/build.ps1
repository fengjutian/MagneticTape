$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$projectRoot = Split-Path -Parent $PSScriptRoot
$desktopRoot = Join-Path $projectRoot "apps\desktop"

Push-Location $desktopRoot
try {
    if (-not (Test-Path "node_modules")) {
        npm ci
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
    npm run tauri build
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $installer = @(Get-ChildItem (Join-Path $projectRoot "target\release\bundle\nsis\*.exe") -File -ErrorAction Stop)
    if ($installer.Count -ne 1) {
        throw "Expected exactly one NSIS installer, found $($installer.Count)"
    }
    $hash = (Get-FileHash -LiteralPath $installer[0].FullName -Algorithm SHA256).Hash.ToLower()
    "$hash  $($installer[0].Name)" | Set-Content -LiteralPath "$($installer[0].FullName).sha256" -Encoding ascii
    Write-Host "Installer: $($installer[0].FullName)"
    Write-Host "SHA-256:   $hash"
} finally {
    Pop-Location
}
