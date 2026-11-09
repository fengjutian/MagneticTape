# Generates placeholder screenshots for the docs site.
# Pure ASCII on purpose: PowerShell 5.1 decodes .ps1 files as ANSI (GBK) on
# Chinese Windows, and non-ASCII literals would break script parsing.
#
# Usage:
#   powershell -File apps/site/scripts/make-placeholder.ps1
#
# Drop a real capture at apps/site/public/screenshots/<name>.png to replace it.

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent $PSScriptRoot
$outDir = Join-Path $root 'public\screenshots'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$width = 1280
$height = 800

function New-Brush([string]$hex, [int]$alpha) {
    $r = [Convert]::ToInt32($hex.Substring(0, 2), 16)
    $g = [Convert]::ToInt32($hex.Substring(2, 2), 16)
    $b = [Convert]::ToInt32($hex.Substring(4, 2), 16)
    return New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb($alpha, $r, $g, $b))
}

function New-RoundedPath([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $path.AddArc($x, $y, $d, $d, 180, 90)
    $path.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $path.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $path.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    return $path
}

function Draw-Panel($g, [float]$x, [float]$y, [float]$w, [float]$h, [string]$label) {
    $path = New-RoundedPath $x $y $w $h 20
    $fill = New-Brush 'FFFFFF' 200
    $g.FillPath($fill, $path)
    $stroke = New-Object System.Drawing.Pen((New-Brush 'FFFFFF' 255), 1)
    $g.DrawPath($stroke, $path)
    $font = New-Object System.Drawing.Font('Segoe UI', 11, [System.Drawing.FontStyle]::Bold)
    $textBrush = New-Brush '8A8FA2' 255
    $g.DrawString($label, $font, $textBrush, $x + 16, $y + 14)
    $font.Dispose()
}

function Draw-Lines($g, [float]$x, [float]$y, [float]$w, [int]$count, [int]$gap) {
    for ($i = 0; $i -lt $count; $i++) {
        $ly = $y + $i * $gap
        $lw = $w * (0.55 + 0.4 * (($i % 3) / 3.0))
        $path = New-RoundedPath $x $ly $lw 9 4
        $brush = New-Brush '232338' 26
        $g.FillPath($brush, $path)
    }
}

function New-Placeholder([string]$name, [string]$caption) {
    $bmp = New-Object System.Drawing.Bitmap($width, $height)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::ClearTypeGridFit

    $g.Clear([System.Drawing.ColorTranslator]::FromHtml('#F4F6FB'))
    foreach ($glow in @(
        @{ x = -120; y = -160; c = '6EE7B7'; a = 70 },
        @{ x = 980; y = -140; c = 'A79BF5'; a = 56 },
        @{ x = 760; y = 620; c = '93C5FD'; a = 52 }
    )) {
        $brush = New-Brush $glow.c $glow.a
        $g.FillEllipse($brush, $glow.x, $glow.y, 760, 520)
    }

    $chrome = New-RoundedPath 24 24 ($width - 48) ($height - 48) 26
    $g.FillPath((New-Brush 'FFFFFF' 150), $chrome)

    # left: recorder controls
    Draw-Panel $g 48 48 340 ($height - 96) 'RECORDER CONTROL'
    Draw-Lines $g 72 96 260 9 26
    # center: transcript
    Draw-Panel $g 408 48 460 ($height - 96) 'LIVE TRANSCRIPT'
    Draw-Lines $g 432 96 380 16 26
    # right: insights
    Draw-Panel $g 888 48 ($width - 936) ($height - 96) 'AI INSIGHTS'
    Draw-Lines $g 912 96 240 10 26

    $font = New-Object System.Drawing.Font('Segoe UI', 15, [System.Drawing.FontStyle]::Bold)
    $g.DrawString('MagneticTape', $font, (New-Brush '128456' 255), 48, ($height - 62))
    $small = New-Object System.Drawing.Font('Segoe UI', 11)
    $g.DrawString($caption, $small, (New-Brush '646A7E' 255), 190, ($height - 60))
    $font.Dispose()
    $small.Dispose()

    $path = Join-Path $outDir "$name.png"
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose()
    $bmp.Dispose()
    Write-Host "wrote $path"
}

New-Placeholder 'recorder' 'SCREENSHOT PLACEHOLDER - recorder control column'
New-Placeholder 'transcript' 'SCREENSHOT PLACEHOLDER - live transcript view'
New-Placeholder 'insights' 'SCREENSHOT PLACEHOLDER - AI insights column'
New-Placeholder 'workspace' 'SCREENSHOT PLACEHOLDER - full workspace'

# Design-doc images: the manifest is written by scripts/sync-design-docs.mjs and
# carries the alt text (Chinese) as UTF-8 JSON, so no non-ASCII literal is needed
# inside this script.
$manifestPath = Join-Path $PSScriptRoot 'design-image-manifest.json'
$designDir = Join-Path $root 'public\screenshots\design'

if (Test-Path $manifestPath) {
    $manifest = [IO.File]::ReadAllText($manifestPath, [Text.Encoding]::UTF8) | ConvertFrom-Json
    New-Item -ItemType Directory -Force -Path $designDir | Out-Null

    foreach ($entry in $manifest) {
        $path = Join-Path $designDir $entry.file
        if (Test-Path $path) { continue }

        $bmp = New-Object System.Drawing.Bitmap(1200, 800)
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAlias
        $g.Clear([System.Drawing.ColorTranslator]::FromHtml('#F4F6FB'))
        foreach ($glow in @(
            @{ x = -140; y = -180; c = '6EE7B7'; a = 66 },
            @{ x = 860; y = -150; c = 'A79BF5'; a = 54 },
            @{ x = 700; y = 560; c = '93C5FD'; a = 50 }
        )) {
            $brush = New-Brush $glow.c $glow.a
            $g.FillEllipse($brush, $glow.x, $glow.y, 820, 560)
        }

        $frame = New-RoundedPath 40 40 1120 720 26
        $g.FillPath((New-Brush 'FFFFFF' 170), $frame)
        $g.DrawPath((New-Object System.Drawing.Pen((New-Brush 'FFFFFF' 255), 1)), $frame)

        $label = [string]$entry.alt
        $font = New-Object System.Drawing.Font('Microsoft YaHei UI', 22)
        $brush2 = New-Brush '232338' 255
        $size = $g.MeasureString($label, $font)
        $g.DrawString($label, $font, $brush2, (600 - $size.Width / 2), (400 - $size.Height / 2))
        $font.Dispose()

        $small = New-Object System.Drawing.Font('Segoe UI', 12)
        $tag = 'MagneticTape design doc - placeholder (' + $entry.file + ')'
        $g.DrawString($tag, $small, (New-Brush '8A8FA2' 255), 40, 726)
        $small.Dispose()

        $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
        $g.Dispose()
        $bmp.Dispose()
        Write-Host "wrote $path"
    }
}
