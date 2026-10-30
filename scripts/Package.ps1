param(
    [string]$VpkPath = (Join-Path (Split-Path $PSScriptRoot -Parent) '.tools\vpk\vpk.exe'),
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
$projectDirectory = Split-Path $PSScriptRoot -Parent
Push-Location $projectDirectory
try {
    $releaseNumber = (Get-Content -LiteralPath VERSION -Raw).Trim()
    if ($releaseNumber -notmatch '^\d{3,}$') { throw 'VERSION must be an integer with at least three digits' }
    $releaseTag = 'v' + $releaseNumber
    $metadata = (& cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages[0]
    if ($metadata.version -ne ('0.0.' + [int]$releaseNumber)) { throw 'Cargo package metadata and VERSION disagree' }
    if (!$SkipBuild) {
        & cargo build --locked --release
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    }
    & (Join-Path $PSScriptRoot 'Third-Party-Notices.ps1')
    $stageDirectory = Join-Path $projectDirectory ('dist\stage-' + $releaseTag)
    $releaseDirectory = Join-Path $projectDirectory ('dist\' + $releaseTag)
    if ((Test-Path -LiteralPath $stageDirectory) -or (Test-Path -LiteralPath $releaseDirectory)) {
        throw 'This version was already packaged. Use a clean workspace or the next VERSION.'
    }
    New-Item -ItemType Directory -Path $stageDirectory,$releaseDirectory -Force | Out-Null
    Copy-Item -LiteralPath 'target\release\sniptory.exe','LICENSE','licenses\THIRD_PARTY_NOTICES.txt','licenses\Slint-Royalty-Free.md' -Destination $stageDirectory
    & $VpkPath pack --packId BazStudios.SnipTory --packVersion $metadata.version --packDir $stageDirectory --mainExe sniptory.exe --packTitle 'SnipTory' --packAuthors 'Baz Studios LLC' --icon 'assets\sniptory.ico' --channel win --runtime win-x64 --shortcuts StartMenuRoot --releaseNotes ('release-notes\' + $releaseTag + '.md') --outputDir $releaseDirectory
    if ($LASTEXITCODE -ne 0) { throw 'Installer packaging failed' }
    # Public filenames use the same integer release sequence as the app. Only
    # tool-required package metadata uses semantic versioning.
    $renamedPackages = @{}
    foreach ($package in (Get-ChildItem -LiteralPath $releaseDirectory -Filter '*.nupkg')) {
        $kind = if ($package.Name -like '*delta*') { 'delta' } else { 'full' }
        $newName = "SnipTory-$releaseTag-$kind.nupkg"
        $renamedPackages[$package.Name] = $newName
        Rename-Item -LiteralPath $package.FullName -NewName $newName
    }
    $feedPath = Join-Path $releaseDirectory 'releases.win.json'
    $feed = Get-Content -LiteralPath $feedPath -Raw | ConvertFrom-Json
    foreach ($asset in $feed.Assets) { if ($renamedPackages.ContainsKey($asset.FileName)) { $asset.FileName = $renamedPackages[$asset.FileName] } }
    $feed | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $feedPath -Encoding utf8
    foreach ($legacy in (Get-ChildItem -LiteralPath $releaseDirectory -Filter 'RELEASES*')) {
        $text = Get-Content -LiteralPath $legacy.FullName -Raw
        foreach ($name in $renamedPackages.Keys) { $text = $text.Replace($name,$renamedPackages[$name]) }
        Set-Content -LiteralPath $legacy.FullName -Value $text -Encoding utf8 -NoNewline
    }
    foreach ($installer in (Get-ChildItem -LiteralPath $releaseDirectory -Filter '*Setup.exe')) { Rename-Item -LiteralPath $installer.FullName -NewName "SnipTory-$releaseTag-Setup.exe" }
    foreach ($portable in (Get-ChildItem -LiteralPath $releaseDirectory -Filter '*Portable.zip')) { Rename-Item -LiteralPath $portable.FullName -NewName "SnipTory-$releaseTag-Portable.zip" }
    $assetsPath = Join-Path $releaseDirectory 'assets.win.json'
    if (Test-Path -LiteralPath $assetsPath) {
        $assets = Get-Content -LiteralPath $assetsPath -Raw | ConvertFrom-Json
        foreach ($asset in $assets) {
            if ($renamedPackages.ContainsKey($asset.RelativeFileName)) { $asset.RelativeFileName = $renamedPackages[$asset.RelativeFileName] }
            elseif ($asset.Type -eq 'Installer') { $asset.RelativeFileName = "SnipTory-$releaseTag-Setup.exe" }
            elseif ($asset.Type -eq 'Portable') { $asset.RelativeFileName = "SnipTory-$releaseTag-Portable.zip" }
        }
        ConvertTo-Json -InputObject @($assets) -Depth 10 | Set-Content -LiteralPath $assetsPath -Encoding utf8
    }
    $checksums = Get-ChildItem -LiteralPath $releaseDirectory -File | Sort-Object Name | ForEach-Object { '{0}  {1}' -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(),$_.Name }
    $checksums | Set-Content -LiteralPath (Join-Path $releaseDirectory 'SHA256SUMS.txt') -Encoding utf8
    Write-Host "Packaged $releaseTag at $releaseDirectory"
} finally { Pop-Location }
