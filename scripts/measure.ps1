# Samples real RAM/CPU for the app's *whole process tree* (riff-rust + WebView2 +
# any yt-dlp/ffmpeg children). No claims, just numbers.
#   npm run measure                      # measures a running riff-rust process
#   npm run measure -- -Seconds 30
#   npm run measure -- -ExePath .\src-tauri\target\release\riff-rust.exe
param(
  [string]$ProcessName = 'riff-rust',
  [int]$Seconds = 15,
  [string]$ExePath = ''
)

$ErrorActionPreference = 'Stop'

if ($ExePath) {
  if (-not (Test-Path $ExePath)) { throw "Executable not found: $ExePath" }
  Start-Process -FilePath (Resolve-Path $ExePath)
  Start-Sleep -Seconds 4
}

$roots = @(Get-Process -Name $ProcessName -ErrorAction SilentlyContinue)
if ($roots.Count -eq 0) {
  throw "No process named '$ProcessName'. Start the app first or pass -ExePath."
}
$rootIds = @($roots | Select-Object -ExpandProperty Id)

# Rebuild the parent map each sample so processes spawned mid-measure are included.
function Get-TreeProcesses {
  $all = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
    Select-Object ProcessId, ParentProcessId, Name)
  $byParent = @{}
  foreach ($p in $all) {
    if (-not $byParent.ContainsKey([int]$p.ParentProcessId)) {
      $byParent[[int]$p.ParentProcessId] = @()
    }
    $byParent[[int]$p.ParentProcessId] += [int]$p.ProcessId
  }
  $wanted = [System.Collections.Generic.HashSet[int]]::new()
  $queue = [System.Collections.Generic.Queue[int]]::new()
  foreach ($id in $rootIds) { $queue.Enqueue($id) }
  while ($queue.Count -gt 0) {
    $id = $queue.Dequeue()
    if (-not $wanted.Add($id)) { continue }
    if ($byParent.ContainsKey($id)) {
      foreach ($child in $byParent[$id]) { $queue.Enqueue($child) }
    }
  }
  $ids = @($wanted)
  $procs = @(Get-Process -Id $ids -ErrorAction SilentlyContinue)
  [pscustomobject]@{
    Processes = $procs
    Names     = @($procs | Select-Object -ExpandProperty ProcessName | Sort-Object -Unique)
  }
}

function Get-Snapshot {
  $tree = Get-TreeProcesses
  $procs = $tree.Processes
  $rootProcs = @($procs | Where-Object { $rootIds -contains $_.Id })
  [pscustomobject]@{
    WorkingSet = ($procs | Measure-Object WorkingSet64 -Sum).Sum
    Private    = ($procs | Measure-Object PrivateMemorySize64 -Sum).Sum
    RootWorkingSet = ($rootProcs | Measure-Object WorkingSet64 -Sum).Sum
    CpuSeconds = ($procs | ForEach-Object { $_.CPU } | Measure-Object -Sum).Sum
    Count      = $procs.Count
    Names      = ($tree.Names -join ', ')
  }
}

$start = Get-Snapshot
$peakWorking = 0
$peakPrivate = 0
for ($i = 0; $i -lt $Seconds; $i++) {
  Start-Sleep -Seconds 1
  $now = Get-Snapshot
  if (-not $now.Count) { throw 'Process exited during measurement.' }
  if ($now.WorkingSet -gt $peakWorking) { $peakWorking = $now.WorkingSet }
  if ($now.Private -gt $peakPrivate) { $peakPrivate = $now.Private }
}
$end = Get-Snapshot
$cores = [Environment]::ProcessorCount
$cpuSeconds = [Math]::Max(0, $end.CpuSeconds - $start.CpuSeconds)

[pscustomobject]@{
  RootProcess          = $ProcessName
  TreeProcesses        = $end.Count
  'Root working set (MB)'  = [Math]::Round($end.RootWorkingSet / 1MB, 1)
  'Tree working set (MB)'  = [Math]::Round($end.WorkingSet / 1MB, 1)
  'Tree working set peak (MB)' = [Math]::Round($peakWorking / 1MB, 1)
  'Tree private peak (MB)' = [Math]::Round($peakPrivate / 1MB, 1)
  'CPU one-core (%)'   = [Math]::Round(($cpuSeconds / $Seconds) * 100, 1)
  'CPU total (%)'      = [Math]::Round(($cpuSeconds / $Seconds / $cores) * 100, 1)
  'Process names'      = $end.Names
} | Format-List
