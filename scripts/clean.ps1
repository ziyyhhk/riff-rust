# Removes build artifacts. `-All` also removes dependency/build caches (slow next build,
# but reclaims the most disk: Rust target/ is usually several GB).
param(
  [switch]$All,
  [switch]$KeepRelease
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

function Get-PathSize([string]$path) {
  if (-not (Test-Path $path)) { return 0 }
  return (Get-ChildItem $path -Recurse -File -ErrorAction SilentlyContinue |
    Measure-Object Length -Sum).Sum
}

function Remove-Path([string]$path, [string]$label) {
  if (-not (Test-Path $path)) { return 0 }
  $size = Get-PathSize $path
  Remove-Item $path -Recurse -Force -ErrorAction SilentlyContinue
  if (Test-Path $path) {
    Write-Host "  skipped (in use): $label"
    return 0
  }
  Write-Host ("  removed {0,-42} {1,9:N1} MB" -f $label, ($size / 1MB))
  return $size
}

$freed = 0
Write-Host 'Riff Rust cleanup'

$freed += Remove-Path (Join-Path $root 'web') 'web/ (assembled frontend)'
if (-not $KeepRelease) {
  $freed += Remove-Path (Join-Path $root 'release') 'release/ (installers)'
}

$logs = Join-Path $env:LOCALAPPDATA 'Riff Rust\logs'
if (Test-Path $logs) {
  Get-ChildItem $logs -File -Filter 'riff-rust.log*' -ErrorAction SilentlyContinue | ForEach-Object {
    $size = $_.Length
    Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue
    if (-not (Test-Path $_.FullName)) {
      Write-Host ("  removed {0,-42} {1,9:N1} MB" -f "logs/$($_.Name)", ($size / 1MB))
      $freed += $size
    }
  }
}

if ($All) {
  $freed += Remove-Path (Join-Path $root 'node_modules') 'node_modules/'
  $freed += Remove-Path (Join-Path $root 'src-tauri\gen') 'src-tauri/gen/ (generated)'
  $freed += Remove-Path (Join-Path $root 'src-tauri\target') 'src-tauri/target/ (Rust build)'
} else {
  # debug/ holds check/test artifacts and is usually the largest folder; the
  # release profile stays so the app still runs and rebuilds stay incremental.
  $freed += Remove-Path (Join-Path $root 'src-tauri\target\debug') 'src-tauri/target/debug/'
  Write-Host '  kept node_modules/ and src-tauri/target/release/ (pass -All to remove them)'
}

Write-Host ("Freed {0:N1} MB" -f ($freed / 1MB))
if ($All) {
  Write-Host 'Next build is a cold build (~10-15 min on this repo).'
}
