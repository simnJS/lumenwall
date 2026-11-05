<#
.SYNOPSIS
  Runs the probe on both backends, interleaved, and prints the result table (Markdown).
  Creates a GPU device: only run it when the PC is free (see README, "Rules").

.EXAMPLE
  pwsh -File tools\d3d11-probe\run-matrix.ps1 -Exe target\release\d3d11-probe.exe -Name ivy
#>
param(
    [Parameter(Mandatory)][string]$Exe,
    [string]$Name = 'probe',
    [int]$Runs = 3,
    [string]$Wallpaper = '3463520581',
    [int]$Seconds = 20,
    [int]$Warmup = 5,
    [string]$Out = (Join-Path $env:TEMP 'd3d11-probe-results')
)
$ErrorActionPreference = 'Stop'
$lockDir = Join-Path $env:LOCALAPPDATA 'lumenwall-dev'
$lock = Join-Path $lockDir 'gpu.lock'
New-Item -ItemType Directory -Force $lockDir, $Out | Out-Null
if (Test-Path $lock) { throw "GPU lock held by: $(Get-Content $lock -Raw)" }
Set-Content -Path $lock -Value $Name
$rows = @()
try {
    for ($r = 1; $r -le $Runs; $r++) {
        # Interleaved; the engine that goes first alternates.
        $order = if ($r % 2 -eq 1) { 'd3d11', 'vulkan' } else { 'vulkan', 'd3d11' }
        foreach ($b in $order) {
            $line = & $Exe run --backend $b --wallpaper $Wallpaper --seconds $Seconds --warmup $Warmup |
                Where-Object { $_ -like 'RESULT *' } | Select-Object -Last 1
            if (-not $line) { throw "run $r $b produced no RESULT line" }
            Add-Content -Path (Join-Path $Out "$Wallpaper.txt") -Value $line
            $o = [ordered]@{ run = $r }
            foreach ($m in [regex]::Matches($line, '(\w+)=("[^"]*"|\S+)')) {
                $o[$m.Groups[1].Value] = $m.Groups[2].Value.Trim('"')
            }
            $rows += [pscustomobject]$o
            Start-Sleep -Seconds 3
        }
    }
}
finally {
    Remove-Item $lock -ErrorAction SilentlyContinue
}
$cols = 'backend', 'run', 'shared_mib', 'dedicated_mib', 'private_mib', 'working_set_mib', 'cpu_percent',
    'gpu_3d_percent', 'create_ms', 'upload_ms', 'shaders_ms', 'compile_ms', 'pipelines_ms', 'late_frames', 'shared_device_mib'
'| ' + ($cols -join ' | ') + ' |'
'|' + (($cols | ForEach-Object { '---' }) -join '|') + '|'
foreach ($row in ($rows | Sort-Object backend, run)) {
    '| ' + (($cols | ForEach-Object { $row.$_ }) -join ' | ') + ' |'
}
