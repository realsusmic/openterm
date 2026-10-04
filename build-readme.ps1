[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$ReadmePath = Join-Path $Root 'README.md'
$AssetDirectory = Join-Path $Root 'assets'
$ChartPath = Join-Path $AssetDirectory 'codebase-composition.svg'

function Get-SourceStats {
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [System.IO.FileInfo[]] $Files
    )

    $uniqueFiles = @($Files | Sort-Object -Property FullName -Unique)
    $lines = 0
    foreach ($file in $uniqueFiles) {
        $lines += @(Get-Content -LiteralPath $file.FullName | Where-Object {
            -not [string]::IsNullOrWhiteSpace($_)
        }).Count
    }

    [pscustomobject]@{
        Name  = $Name
        Files = $uniqueFiles.Count
        Lines = $lines
    }
}

$rustFiles = @(
    Get-ChildItem -LiteralPath (Join-Path $Root 'src') -Recurse -File -Filter '*.rs'
    Get-Item -LiteralPath (Join-Path $Root 'build.rs')
)
$goFiles = @(Get-ChildItem -LiteralPath (Join-Path $Root 'agent') -Recurse -File -Filter '*.go')
$cFiles = @(Get-ChildItem -LiteralPath (Join-Path $Root 'native') -Recurse -File | Where-Object {
    $_.Extension -in '.c', '.h'
})
$zigFiles = @(Get-ChildItem -LiteralPath (Join-Path $Root 'native') -Recurse -File -Filter '*.zig')
$stats = @(
    Get-SourceStats 'Rust' $rustFiles
    Get-SourceStats 'Go' $goFiles
    Get-SourceStats 'C' $cFiles
    Get-SourceStats 'Zig' $zigFiles
) | Where-Object { $_.Lines -gt 0 }

$totalLines = ($stats | Measure-Object -Property Lines -Sum).Sum
if (-not $totalLines) {
    throw 'No source lines were found.'
}

# Mermaid labels every pie slice, which makes adjacent one- and two-percent
# labels overlap. Generate a small self-contained SVG instead: all languages
# stay in the legend, while slice labels below three percent are omitted.
$colors = @('#dea584', '#00add8', '#555555', '#f7a41d')
$culture = [System.Globalization.CultureInfo]::InvariantCulture
$cx = 190.0
$cy = 180.0
$radius = 132.0
$angle = -90.0
$svg = [System.Collections.Generic.List[string]]::new()
$svg.Add('<svg xmlns="http://www.w3.org/2000/svg" width="720" height="360" viewBox="0 0 720 360" role="img" aria-labelledby="title desc">')
$svg.Add('  <title id="title">OpenTerm source composition</title>')
$svg.Add('  <desc id="desc">Pie chart of non-empty source lines in Rust, Go, C, and Zig. Build scripts are excluded.</desc>')
$svg.Add('  <rect width="720" height="360" rx="12" fill="#0d1117"/>')

for ($index = 0; $index -lt $stats.Count; $index++) {
    $item = $stats[$index]
    $share = 100.0 * $item.Lines / $totalLines
    $sweep = 360.0 * $item.Lines / $totalLines
    $endAngle = $angle + $sweep
    $startRadians = $angle * [Math]::PI / 180.0
    $endRadians = $endAngle * [Math]::PI / 180.0
    $x1 = $cx + $radius * [Math]::Cos($startRadians)
    $y1 = $cy + $radius * [Math]::Sin($startRadians)
    $x2 = $cx + $radius * [Math]::Cos($endRadians)
    $y2 = $cy + $radius * [Math]::Sin($endRadians)
    $largeArc = 0
    if ($sweep -gt 180.0) { $largeArc = 1 }
    $path = 'M {0} {1} L {2} {3} A {4} {4} 0 {5} 1 {6} {7} Z' -f @(
        $cx.ToString('0.###', $culture),
        $cy.ToString('0.###', $culture),
        $x1.ToString('0.###', $culture),
        $y1.ToString('0.###', $culture),
        $radius.ToString('0.###', $culture),
        $largeArc,
        $x2.ToString('0.###', $culture),
        $y2.ToString('0.###', $culture)
    )
    $svg.Add(('  <path d="{0}" fill="{1}" stroke="#0d1117" stroke-width="2"/>' -f $path, $colors[$index]))

    if ($share -ge 3.0) {
        $middleRadians = ($angle + ($sweep / 2.0)) * [Math]::PI / 180.0
        $labelRadius = $radius * 0.62
        $labelX = $cx + $labelRadius * [Math]::Cos($middleRadians)
        $labelY = $cy + $labelRadius * [Math]::Sin($middleRadians)
        $svg.Add(('  <text x="{0}" y="{1}" fill="#ffffff" font-family="Segoe UI, sans-serif" font-size="16" font-weight="600" text-anchor="middle" dominant-baseline="middle">{2:N1}%</text>' -f $labelX.ToString('0.###', $culture), $labelY.ToString('0.###', $culture), $share))
    }

    $legendY = 112 + ($index * 48)
    $svg.Add(('  <rect x="390" y="{0}" width="18" height="18" rx="3" fill="{1}"/>' -f ($legendY - 14), $colors[$index]))
    $svg.Add(('  <text x="420" y="{0}" fill="#f0f6fc" font-family="Segoe UI, sans-serif" font-size="17">{1}</text>' -f $legendY, $item.Name))
    $svg.Add(('  <text x="550" y="{0}" fill="#8b949e" font-family="Segoe UI, sans-serif" font-size="15">{1:N1}% &#183; {2:N0} lines</text>' -f $legendY, $share, $item.Lines))
    $angle = $endAngle
}

$svg.Add('</svg>')
if (-not (Test-Path -LiteralPath $AssetDirectory)) {
    New-Item -ItemType Directory -Path $AssetDirectory | Out-Null
}
$utf8NoBom = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText($ChartPath, ($svg -join "`n") + "`n", $utf8NoBom)

$generated = [System.Collections.Generic.List[string]]::new()
$generated.Add('<!-- codebase-chart:start -->')
$generated.Add('## codebase composition')
$generated.Add('')
$generated.Add('Generated by `build-readme.bat` from non-empty Rust, Go, C, and Zig source lines; build scripts, outputs, and dependencies are excluded.')
$generated.Add('')
$generated.Add('![OpenTerm source composition](assets/codebase-composition.svg)')
$generated.Add('')
$generated.Add('| Language | Files | Non-empty lines | Share |')
$generated.Add('|----------|------:|----------------:|------:|')
foreach ($item in $stats) {
    $share = 100.0 * $item.Lines / $totalLines
    $generated.Add(('| {0} | {1} | {2} | {3:N1}% |' -f $item.Name, $item.Files, $item.Lines, $share))
}
$generated.Add(('| **Total** | **{0}** | **{1}** | **100.0%** |' -f (($stats | Measure-Object -Property Files -Sum).Sum), $totalLines))
$generated.Add('<!-- codebase-chart:end -->')
$section = $generated -join "`n"

$readme = [System.IO.File]::ReadAllText($ReadmePath)
$pattern = '(?s)<!-- codebase-chart:start -->.*?<!-- codebase-chart:end -->'
if ([regex]::IsMatch($readme, $pattern)) {
    $readme = [regex]::Replace($readme, $pattern, $section, 1)
} elseif ($readme.Contains('## layout')) {
    $readme = $readme.Replace('## layout', "$section`n`n## layout")
} else {
    $readme = $readme.TrimEnd() + "`n`n$section`n"
}

[System.IO.File]::WriteAllText($ReadmePath, $readme, $utf8NoBom)

Write-Host ('[readme] {0} files, {1} non-empty lines' -f (($stats | Measure-Object -Property Files -Sum).Sum), $totalLines)
