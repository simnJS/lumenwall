#Requires -Version 7.2
<#
.SYNOPSIS
    Turns the raw files of one bench-vs-we.ps1 run into results.csv and summary.md.

.DESCRIPTION
    Reads machine.json, runs.csv, samples.csv, processes.csv and restore.json from
    -OutDir. A (wallpaper, engine) value is the mean of its run means; the spread is the range of
    the run means when there are several repetitions, otherwise the standard deviation of the
    one-second samples of the single run (noise within the run, not run-to-run variation).
#>
[CmdletBinding()]
param([Parameter(Mandatory)] [string] $OutDir)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
$inv = [Globalization.CultureInfo]::InvariantCulture

function Num($v) { if ($null -eq $v -or "$v" -eq '' -or "$v" -match 'N/A') { return $null } [double]::Parse("$v".Trim(), $inv) }
# Safe property read and statistic field (StrictMode: missing properties throw).
function G($o, [string] $n) { if ($o -and $o.PSObject.Properties.Name -contains $n) { $o.$n } else { $null } }
function M($s, [string] $f = 'Mean') { if ($null -eq $s) { $null } else { $s.$f } }
function F($v, [int] $d = 1) { if ($null -eq $v) { return '–' } ([double]$v).ToString("N$d", $inv) }
function Stats([double[]] $xs) {
    if (-not $xs -or $xs.Count -eq 0) { return $null }
    $mean = ($xs | Measure-Object -Average).Average
    $sd = if ($xs.Count -gt 1) { [Math]::Sqrt((($xs | ForEach-Object { ($_ - $mean) * ($_ - $mean) }) | Measure-Object -Sum).Sum / ($xs.Count - 1)) } else { 0 }
    [pscustomobject]@{ Mean = $mean; Sd = $sd; Min = ($xs | Measure-Object -Minimum).Minimum; Max = ($xs | Measure-Object -Maximum).Maximum; N = $xs.Count }
}

$machine = Get-Content (Join-Path $OutDir 'machine.json') -Raw | ConvertFrom-Json
$runs = @(Import-Csv (Join-Path $OutDir 'runs.csv'))
$samples = @(Import-Csv (Join-Path $OutDir 'samples.csv'))
$procs = @(if (Test-Path (Join-Path $OutDir 'processes.csv')) { Import-Csv (Join-Path $OutDir 'processes.csv') })
$restore = if (Test-Path (Join-Path $OutDir 'restore.json')) { Get-Content (Join-Path $OutDir 'restore.json') -Raw | ConvertFrom-Json } else { $null }

$tags = @($machine.adapters | Where-Object { $_.tag -ne 'basic' } | ForEach-Object tag)
$metricCols = @('cpu_pct', 'private_mib', 'ws_mib', 'processes', 'ui_cpu_pct', 'other_cpu_pct', 'system_cpu_pct', 'nv_power_w', 'nv_util_pct', 'nv_dec_pct', 'nv_clock_mhz', 'nv_mem_mib')
foreach ($t in $tags) { $metricCols += @("gpu3d_pct_$t", "gpu_vdec_pct_$t", "gpu_copy_pct_$t", "gpu_all_pct_$t", "vram_ded_mib_$t", "vram_shared_mib_$t") }

# Per run: statistics of each metric over its samples.
function RunStats($run, [string] $engine) {
    $rows = @($samples | Where-Object { $_.run -eq $run.Run -and ($engine -eq '' -or $_.engine -eq $engine) })
    $res = [ordered]@{ Run = $run.Run; Kind = $run.Kind; Engine = $(if ($engine) { $engine } else { $run.Engine }); Wallpaper = $run.Wallpaper; Samples = $rows.Count }
    foreach ($m in $metricCols) {
        if (-not ($rows.Count -and $rows[0].PSObject.Properties.Name -contains $m)) { continue }
        $res[$m] = Stats ([double[]]@($rows | ForEach-Object { Num $_.$m } | Where-Object { $null -ne $_ }))
    }
    $res.FirstGpuMs = Num $run.FirstGpuMs
    $res.CommandMs = Num $run.CommandMs
    $res.LwStatus = $run.LwStatus
    $res.LwFps = Num $run.LwFps
    $res.LwOutputs = $run.LwOutputs
    $res.Error = G $run 'Error'
    [pscustomobject]$res
}

$runStats = @($runs | Where-Object Kind -EQ 'run' | ForEach-Object { RunStats $_ '' })
$idleStats = @($runs | Where-Object Kind -EQ 'idle' | ForEach-Object {
        $r = $_
        if ($r.Run -eq 'idle-resident') { RunStats $r 'we'; RunStats $r 'lumenwall' } else { RunStats $r '' }
    })

# (wallpaper, engine) aggregate.
$wallpapers = @($machine.wallpapers)
$engines = @('we', 'lumenwall')
$agg = @{}
foreach ($wp in $wallpapers) {
    foreach ($e in $engines) {
        $rs = @($runStats | Where-Object { $_.Wallpaper -eq $wp.id -and $_.Engine -eq $e })
        $a = [ordered]@{ wallpaper = $wp.id; type = $wp.type; engine = $e; runs = $rs.Count }
        foreach ($m in $metricCols) {
            $means = [double[]]@($rs | Where-Object { $_.PSObject.Properties.Name -contains $m -and $_.$m } | ForEach-Object { M $_.$m })
            if ($means.Count -eq 0) { continue }
            $mean = ($means | Measure-Object -Average).Average
            $spread = if ($rs.Count -gt 1) { (($means | Measure-Object -Maximum).Maximum - ($means | Measure-Object -Minimum).Minimum) / 2 } else { M (G $rs[0] $m) 'Sd' }
            $a[$m] = $mean; $a["${m}_spread"] = $spread
            $a["${m}_max"] = (@($rs | ForEach-Object { M (G $_ $m) 'Max' } | Where-Object { $null -ne $_ }) | Measure-Object -Maximum).Maximum
        }
        $first = @($rs | ForEach-Object FirstGpuMs | Where-Object { $null -ne $_ })
        $a.first_gpu_ms = if ($first.Count) { ($first | Measure-Object -Average).Average } else { $null }
        $fpsList = @($rs | ForEach-Object LwFps | Where-Object { $null -ne $_ })
        $a.lw_fps = if ($e -eq 'lumenwall' -and $fpsList.Count) { ($fpsList | Measure-Object -Average).Average } else { $null }
        $a.lw_status = if ($e -eq 'lumenwall') { ($rs | ForEach-Object LwStatus | Select-Object -Unique) -join ' | ' } else { $null }
        $a.error = (@($rs | ForEach-Object Error | Where-Object { $_ }) | Select-Object -Unique) -join ' | '
        $agg["$($wp.id)|$e"] = [pscustomobject]$a
    }
}
$agg.Values | Sort-Object wallpaper, engine | Select-Object * | ForEach-Object {
    $o = [ordered]@{}
    foreach ($p in $_.PSObject.Properties) { $o[$p.Name] = if ($p.Value -is [double]) { [Math]::Round($p.Value, 3) } else { $p.Value } }
    [pscustomobject]$o
} | Export-Csv (Join-Path $OutDir 'results.csv') -NoTypeInformation -Encoding utf8

# ------------------------------------------------------------------------------- summary.md
$md = [Collections.Generic.List[string]]::new()
function Line([string] $s = '') { $md.Add($s) }
function Cell($a, [string] $m, [int] $d) {
    if (-not $a) { return '–' }
    if (-not ($a.PSObject.Properties.Name -contains $m)) { if ($a.error) { return 'not run' } return '–' }
    $v = $a.$m; $s = $a."${m}_spread"
    if ($null -eq $v) { return '–' }
    if ($null -eq $s -or $s -eq 0) { return (F $v $d) }
    "$(F $v $d) ± $(F $s $d)"
}
function Ratio($we, $lw, [string] $m) {
    if (-not $we -or -not $lw -or -not ($we.PSObject.Properties.Name -contains $m) -or -not ($lw.PSObject.Properties.Name -contains $m)) { return '–' }
    if ([Math]::Abs($we.$m) -lt 1e-9) { return '–' }
    '×' + (F ($lw.$m / $we.$m) 2)
}
function MetricTable([string] $title, [string] $m, [int] $d, [string] $unit) {
    Line "### $title"
    Line
    Line "| Wallpaper | Type | Wallpaper Engine ($unit) | Lumenwall ($unit) | Lumenwall / WE |"
    Line '|---|---|---|---|---|'
    foreach ($wp in $wallpapers) {
        $we = $agg["$($wp.id)|we"]; $lw = $agg["$($wp.id)|lumenwall"]
        Line "| $($wp.id) | $($wp.type) | $(Cell $we $m $d) | $(Cell $lw $m $d) | $(Ratio $we $lw $m) |"
    }
    Line
}

Line "# bench-vs-we summary ($($machine.date))"
Line
Line "- Machine: $($machine.cpu) ($($machine.logical_cpus) threads), $($machine.os)"
foreach ($a in $machine.adapters) { Line "- Adapter $($a.tag): $($a.name), LUID $($a.luid), outputs: $(if ($a.outputs) { $a.outputs -join '; ' } else { 'none' })" }
Line "- NVIDIA: $($machine.nvidia)"
Line "- Lumenwall: $($machine.lumenwall) ($($machine.lumenwall_exe))"
Line "- Wallpaper Engine: $($machine.we_version) ($($machine.we_exe))"
Line "- Wallpaper Engine settings: $(($machine.we_settings.PSObject.Properties | ForEach-Object { "$($_.Name)=$($_.Value)" }) -join ', ')"
Line "- Monitors covered: Wallpaper Engine ``-monitor $((G $machine.parameters 'we_monitors') -join ',')``, Lumenwall $(if ((G $machine.parameters 'lumenwall_outputs') -and ((G $machine.parameters 'lumenwall_outputs') -join ',') -ne 'all') { '`-r ' + ((G $machine.parameters 'lumenwall_outputs') -join ',') + '`' } else { 'every monitor' })"
foreach ($dd in @(G $machine 'displays')) { $p = "$dd" -split '\|'; if ($p.Count -ge 5) { Line "- Display $($p[0]): driven by $($p[1]), monitor $(($p[2] -split '\\')[1]), $($p[4])$(if ($p[3] -eq 'True') { ', primary' })" } }
Line "- fps cap for both: $($machine.fps); repetitions $($machine.parameters.repetitions), warm-up $($machine.parameters.warmup_s) s, measurement $($machine.parameters.measure_s) s, idle $($machine.parameters.idle_s) s"
Line
Line 'Values: mean ± spread (several repetitions: half the range of the run means; one repetition: standard deviation of the 1 s samples). CPU: 100 % = one logical core. GPU: share of time the engine type is busy, summed over the process tree.'
Line

MetricTable 'CPU, process tree' 'cpu_pct' 2 '%'
MetricTable 'Private bytes, process tree' 'private_mib' 0 'MiB'
MetricTable 'Working set, process tree' 'ws_mib' 0 'MiB'
foreach ($t in $tags) {
    MetricTable "GPU 3D, $t" "gpu3d_pct_$t" 2 '%'
    MetricTable "GPU video decode, $t" "gpu_vdec_pct_$t" 2 '%'
    MetricTable "GPU all engines, $t" "gpu_all_pct_$t" 2 '%'
    MetricTable "Dedicated GPU memory, $t" "vram_ded_mib_$t" 0 'MiB'
    MetricTable "Shared GPU memory, $t" "vram_shared_mib_$t" 0 'MiB'
}
MetricTable 'NVIDIA board power (NVML, whole GPU)' 'nv_power_w' 1 'W'
MetricTable 'NVIDIA utilization (NVML, whole GPU)' 'nv_util_pct' 1 '%'
MetricTable 'NVIDIA decoder utilization (NVML)' 'nv_dec_pct' 1 '%'
MetricTable 'NVIDIA graphics clock (NVML)' 'nv_clock_mhz' 0 'MHz'
MetricTable 'NVIDIA memory used (NVML, whole GPU)' 'nv_mem_mib' 0 'MiB'
MetricTable 'System CPU (whole machine, 100 % = all threads)' 'system_cpu_pct' 2 '%'
MetricTable 'Processes in the tree' 'processes' 0 'count'

Line '### Runs that could not run'
Line
Line '| Run | Error |'
Line '|---|---|'
foreach ($r in ($runStats | Where-Object { $_.Error })) { Line "| $($r.Run) | $($r.Error) |" }
Line

Line '### Command → first GPU activity'
Line
Line '| Wallpaper | Wallpaper Engine (ms) | Lumenwall (ms) |'
Line '|---|---|---|'
foreach ($wp in $wallpapers) { Line "| $($wp.id) | $(F $agg["$($wp.id)|we"].first_gpu_ms 0) | $(F $agg["$($wp.id)|lumenwall"].first_gpu_ms 0) |" }
Line

Line '### Lumenwall self-report (IPC get_stats, 1 s after the window)'
Line
Line '| Run | Status | fps (sum of outputs) | Per output |'
Line '|---|---|---|---|'
foreach ($r in ($runStats | Where-Object Engine -EQ 'lumenwall')) { Line "| $($r.Run) | $($r.LwStatus) | $(F $r.LwFps 1) | $($r.LwOutputs) |" }
Line

Line '### Adapters used (3D > 0.5 % or dedicated memory > 1 MiB, run means)'
Line
Line '| Run | Adapters |'
Line '|---|---|'
foreach ($r in $runStats) {
    $used = foreach ($t in $tags) {
        $g = M (G $r "gpu3d_pct_$t"); $v = M (G $r "vram_ded_mib_$t")
        if (($null -ne $g -and $g -gt 0.5) -or ($null -ne $v -and $v -gt 1)) { "$t (3D $(F $g 1) %, $(F $v 0) MiB)" }
    }
    Line "| $($r.Run) | $(if ($used) { $used -join ', ' } else { 'none' }) |"
}
Line

Line '### Idle baselines'
Line
Line '| Phase | Engine | Engine CPU % | Engine private MiB | Engine dedicated MiB (nvidia) | System CPU % | NVIDIA W | NVIDIA util % | NVIDIA MiB |'
Line '|---|---|---|---|---|---|---|---|---|'
foreach ($r in $idleStats) {
    $ded = F (M (G $r 'vram_ded_mib_nvidia')) 0
    Line "| $($r.Run) | $(if ($r.Engine) { $r.Engine } else { 'none' }) | $(F (M (G $r 'cpu_pct')) 2) | $(F (M (G $r 'private_mib')) 0) | $ded | $(F (M (G $r 'system_cpu_pct')) 2) | $(F (M (G $r 'nv_power_w')) 1) | $(F (M (G $r 'nv_util_pct')) 1) | $(F (M (G $r 'nv_mem_mib')) 0) |"
}
Line

if ($procs.Count) {
    Line '### Processes per run (whole window)'
    Line
    Line '| Run | Process | Excluded (UI) | CPU % | Private MiB | 3D nvidia % |'
    Line '|---|---|---|---|---|---|'
    foreach ($p in $procs) {
        $g = if ($p.PSObject.Properties.Name -contains 'gpu3d_pct_nvidia') { $p.gpu3d_pct_nvidia } else { '–' }
        Line "| $($p.run) | $($p.name) ($($p.pid)) | $($p.excluded_ui) | $($p.cpu_pct) | $($p.private_mib) | $g |"
    }
    Line
}

$cov = @($samples | Where-Object { $_.covering } | Select-Object -ExpandProperty covering -Unique)
Line '### Conditions'
Line
Line "- Samples with a maximized or fullscreen window: $(@($samples | Where-Object { $_.covering }).Count) of $($samples.Count)$(if ($cov) { " ($($cov -join '; '))" })"
if ($restore) {
    Line "- Highest audio peak of the engines' sessions: $($restore.max_audio_peak)"
    Line "- Restore: $(($restore.PSObject.Properties | Where-Object Name -NE 'notes' | ForEach-Object { "$($_.Name)=$($_.Value)" }) -join ', ')"
    foreach ($n in $restore.notes) { Line "- Note: $n" }
}
$md | Set-Content (Join-Path $OutDir 'summary.md') -Encoding utf8
Write-Host "summary: $(Join-Path $OutDir 'summary.md')"
