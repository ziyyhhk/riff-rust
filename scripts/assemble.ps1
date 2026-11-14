# Assembles the flat web/ folder the Tauri webview will serve.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$web = Join-Path $root 'web'

if (Test-Path $web) { Remove-Item $web -Recurse -Force }
New-Item -ItemType Directory -Path $web | Out-Null

Copy-Item (Join-Path $root 'src\renderer\*') $web -Recurse -Force
Copy-Item (Join-Path $root 'assets') (Join-Path $web 'assets') -Recurse -Force
Copy-Item (Join-Path $root 'vendor') (Join-Path $web 'vendor') -Recurse -Force

$files = Get-ChildItem $web -Recurse -File | Where-Object { $_.Extension -in '.html', '.js', '.css', '.json' }
foreach ($f in $files) {
  $text = [System.IO.File]::ReadAllText($f.FullName)
  $orig = $text
  $text = $text.Replace('../../assets/', 'assets/').Replace('../../vendor/', 'vendor/')
  $text = $text.Replace('../../src/renderer/defaults.json', 'defaults.json')
  if ($text -ne $orig) { [System.IO.File]::WriteAllText($f.FullName, $text) }
}

# Branding: user-facing app name
$index = Join-Path $web 'index.html'
$t = [System.IO.File]::ReadAllText($index)
$t = $t -replace '<title>[^<]*</title>', '<title>Riff Rust</title>'
[System.IO.File]::WriteAllText($index, $t)

Write-Host "Assembled web/ ($(($files | Measure-Object).Count) files rewritten)"
