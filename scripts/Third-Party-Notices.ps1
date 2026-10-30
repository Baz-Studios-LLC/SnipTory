param([string]$OutputPath = (Join-Path (Split-Path $PSScriptRoot -Parent) 'licenses\THIRD_PARTY_NOTICES.txt'))
$ErrorActionPreference = 'Stop'
$metadataText = & cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'Could not read dependency metadata' }
$metadata = $metadataText | ConvertFrom-Json
$notices = [System.Text.StringBuilder]::new()
[void]$notices.AppendLine('SnipTory third-party notices')
[void]$notices.AppendLine('SnipTory source is MIT. Dependencies retain their respective licenses.')
[void]$notices.AppendLine('Slint components are used under the Slint Royalty-free License. Attribution appears on the repository and release download pages.')
foreach ($package in ($metadata.packages | Where-Object source | Sort-Object name,version)) {
    [void]$notices.AppendLine("`n============================================================")
    [void]$notices.AppendLine(('{0} {1}' -f $package.name,$package.version))
    [void]$notices.AppendLine(('License: {0}' -f $package.license))
    [void]$notices.AppendLine(('Source: {0}' -f $package.repository))
    $packageDirectory = Split-Path $package.manifest_path -Parent
    foreach ($licenseFile in (Get-ChildItem -LiteralPath $packageDirectory -File | Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE|COPYRIGHT)' })) {
        [void]$notices.AppendLine((Get-Content -LiteralPath $licenseFile.FullName -Raw))
    }
}
[System.IO.File]::WriteAllText($OutputPath,$notices.ToString())
