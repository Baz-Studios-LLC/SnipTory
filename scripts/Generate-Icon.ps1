$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$projectRoot = Split-Path $PSScriptRoot -Parent
$assetDirectory = Join-Path $projectRoot 'assets'
New-Item -ItemType Directory -Path $assetDirectory -Force | Out-Null
$iconImages = @()
foreach ($iconSize in @(16, 24, 32, 48, 64, 128, 256)) {
    $bitmap = [System.Drawing.Bitmap]::new($iconSize, $iconSize)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.ScaleTransform($iconSize / 32.0, $iconSize / 32.0)
    $shape = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $shape.AddArc(1, 1, 14, 14, 180, 90)
    $shape.AddArc(17, 1, 14, 14, 270, 90)
    $shape.AddArc(17, 17, 14, 14, 0, 90)
    $shape.AddArc(1, 17, 14, 14, 90, 90)
    $shape.CloseFigure()
    $brush = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml('#162F55'))
    $graphics.FillPath($brush, $shape)
    $pen = [System.Drawing.Pen]::new([System.Drawing.Color]::White, 1.7)
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $graphics.DrawLines($pen, [System.Drawing.PointF[]]@([System.Drawing.PointF]::new(7, 12), [System.Drawing.PointF]::new(7, 7), [System.Drawing.PointF]::new(12, 7)))
    $graphics.DrawLines($pen, [System.Drawing.PointF[]]@([System.Drawing.PointF]::new(20, 7), [System.Drawing.PointF]::new(25, 7), [System.Drawing.PointF]::new(25, 12)))
    $graphics.DrawLines($pen, [System.Drawing.PointF[]]@([System.Drawing.PointF]::new(7, 20), [System.Drawing.PointF]::new(7, 25), [System.Drawing.PointF]::new(12, 25)))
    $graphics.DrawLines($pen, [System.Drawing.PointF[]]@([System.Drawing.PointF]::new(20, 25), [System.Drawing.PointF]::new(25, 25), [System.Drawing.PointF]::new(25, 20)))
    $graphics.DrawRectangle($pen, 12, 12, 8, 8)
    $stream = [System.IO.MemoryStream]::new()
    $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    $iconImages += ,@($iconSize, $stream.ToArray())
    if ($iconSize -eq 256) { $bitmap.Save((Join-Path $assetDirectory 'sniptory.png'), [System.Drawing.Imaging.ImageFormat]::Png) }
    $stream.Dispose(); $pen.Dispose(); $brush.Dispose(); $shape.Dispose(); $graphics.Dispose(); $bitmap.Dispose()
}
$iconFile = [System.IO.File]::Create((Join-Path $assetDirectory 'sniptory.ico'))
$writer = [System.IO.BinaryWriter]::new($iconFile)
$writer.Write([uint16]0); $writer.Write([uint16]1); $writer.Write([uint16]$iconImages.Count)
$iconOffset = 6 + 16 * $iconImages.Count
foreach ($iconImage in $iconImages) {
    $dimension = if ($iconImage[0] -eq 256) { 0 } else { $iconImage[0] }
    $writer.Write([byte]$dimension); $writer.Write([byte]$dimension); $writer.Write([byte]0); $writer.Write([byte]0)
    $writer.Write([uint16]1); $writer.Write([uint16]32)
    $writer.Write([uint32]$iconImage[1].Length); $writer.Write([uint32]$iconOffset)
    $iconOffset += $iconImage[1].Length
}
foreach ($iconImage in $iconImages) { $writer.Write([byte[]]$iconImage[1]) }
$writer.Dispose()
