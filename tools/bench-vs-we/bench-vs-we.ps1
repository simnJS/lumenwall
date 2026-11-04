#Requires -Version 7.2
<#
.SYNOPSIS
    Measures Lumenwall and Wallpaper Engine on the same wallpapers, on the real Windows desktop.

.DESCRIPTION
    Both engines stay resident for the whole run and are treated alike: each run hands one
    wallpaper to the engine (Wallpaper Engine: `-control openWallpaper` on every monitor;
    Lumenwall: a second `lumenwall.exe <wallpaper>` that forwards it to the bench daemon over
    IPC), waits for the warm-up, samples once per second during the measurement, then removes
    the wallpaper again (`-control closeWallpaper` / IPC `clear`). The engines alternate which
    goes first on each wallpaper, and the order flips on every repetition.

    Metrics, all black-box: CPU of the engine's process tree (children included; the UIs
    wallpaperui.exe and lumenwall-ui.exe and their children excluded), private bytes and working
    set, GPU engine running time per adapter (GPU Engine counters, summed over the tree's pids),
    dedicated and shared GPU memory (GPU Process Memory counters), NVIDIA power, utilization and
    clocks (NVML, the library nvidia-smi reads), the time from the command to the first GPU
    activity, and the peak of the engines' audio sessions (to prove nothing was audible). Two
    idle baselines are taken with
    neither engine running (start and end), one with both engines resident and empty.

    The user's state is saved first and restored in a `finally` block: Wallpaper Engine's
    config.json (restored once Wallpaper Engine is closed, because it rewrites its monitor map
    at start-up), Lumenwall's config.toml and ui.json, and the Lumenwall daemon (stopped
    gracefully, relaunched with no arguments at the end). See README.md.

.EXAMPLE
    # The short run on the main monitor only (as in docs/benchmarks/lumenwall-vs-wallpaper-engine.md).
    pwsh -File tools\bench-vs-we\bench-vs-we.ps1 -WeMonitors 0 -LumenwallOutputs DISPLAY1 -RelaunchLumenwall

.EXAMPLE
    # The same on every monitor.
    pwsh -File tools\bench-vs-we\bench-vs-we.ps1 -RelaunchLumenwall

.EXAMPLE
    # The full method: 10 wallpapers, 2 repetitions, 20 s warm-up, 60 s measurement.
    pwsh -File tools\bench-vs-we\bench-vs-we.ps1 -Repetitions 2 -WarmupSeconds 20 -MeasureSeconds 60 -IdleSeconds 60 `
        -Wallpapers 1712475860,1960765686,2234978675,3018340632,3044659344,3089056751,3372348904,3463520581,3044696375,3655057993
#>
[CmdletBinding()]
param(
    # Workshop IDs (folders of -WorkshopDir) or wallpaper folders.
    [string[]] $Wallpapers = @('3463520581', '3018340632', '3044696375', '3655057993'),
    [ValidateRange(1, 100)] [int] $Repetitions = 1,
    [ValidateRange(0, 3600)] [int] $WarmupSeconds = 10,
    [ValidateRange(1, 3600)] [int] $MeasureSeconds = 30,
    [ValidateRange(1, 3600)] [int] $IdleSeconds = 15,
    # Pause after a wallpaper is removed, before the next command.
    [ValidateRange(0, 60)] [int] $SettleSeconds = 3,
    # Frame-rate cap for both engines; 0 = Wallpaper Engine's own setting (passed to Lumenwall).
    [ValidateRange(0, 1000)] [int] $Fps = 0,
    [string] $LumenwallExe = (Join-Path $env:USERPROFILE 'Desktop\lumenwall-0.1.0-x86_64-pc-windows-msvc\lumenwall.exe'),
    # The user's own daemon, relaunched at the end. Default: the executable of the daemon running
    # at the start, else -LumenwallExe. Set it when -LumenwallExe is a development build, so that
    # the build under test never replaces the user's daemon.
    [string] $UserLumenwallExe,
    # LOCALAPPDATA of the bench daemon and its client commands: its caches (shaders, pipelines,
    # Vulkan driver choice) and logs then stay out of the user's %LOCALAPPDATA%\Lumenwall. Empty =
    # the user's own folder (the installed release shares its caches, as in the first report).
    [string] $LumenwallLocalAppData,
    # Show every wallpaper once on the Lumenwall bench daemon (5 s each) before the measured runs,
    # so that a fresh -LumenwallLocalAppData has its shader and pipeline caches filled, as the
    # user's installed copy and Wallpaper Engine's own caches are.
    [switch] $WarmLumenwall,
    # Extra environment of the Lumenwall bench daemon, 'NAME=value' (diagnostic switches).
    [string[]] $LumenwallEnv = @(),
    # Engines measured ('we', 'lumenwall'). With only 'we', no Lumenwall bench daemon starts.
    # (Wallpaper Engine's `-playInWindow` is not offered: `-control mute` does not reach a
    # wallpaper played in a window, so it would be audible.)
    [string[]] $Engines = @('we', 'lumenwall'),
    [string] $WeDir = 'C:\Program Files (x86)\Steam\steamapps\common\wallpaper_engine',
    [string] $WorkshopDir = 'C:\Program Files (x86)\Steam\steamapps\workshop\content\431960',
    # Key of the user section in Wallpaper Engine's config.json.
    [string] $WeUser = $env:USERNAME,
    # Wallpaper Engine monitor indexes to cover (`-monitor N`).
    [string[]] $WeMonitors = @('0', '1'),
    # Lumenwall outputs to cover (`-r <output> --bg <wallpaper>`, e.g. DISPLAY1); empty = every
    # monitor (the wallpaper alone on the command line). Keep it on the same monitors as -WeMonitors.
    [string[]] $LumenwallOutputs = @(),
    [string] $OutDir = (Join-Path ([IO.Path]::GetTempPath()) ('lumenwall-bench-vs-we\' + (Get-Date -Format 'yyyyMMdd-HHmmss'))),
    # Private IPC endpoint of the bench daemon, so no other Lumenwall client can reach it.
    [string] $IpcEndpoint = 'lumenwall-bench-vs-we',
    # Start the user's daemon (lumenwall.exe, no arguments) at the end even when it was not
    # running at the start.
    [switch] $RelaunchLumenwall,
    # Leave Wallpaper Engine running at the end (default: closed, the way it is found or not).
    [switch] $KeepWallpaperEngine,
    # How long to wait for maximized or fullscreen windows to go before starting.
    [ValidateRange(0, 3600)] [int] $CoverWaitSeconds = 180,
    # Start even when maximized or fullscreen windows remain (smoke tests only: Wallpaper Engine
    # pauses for them and Lumenwall pauses the output they cover). They are logged per sample.
    [switch] $AllowCoveringWindows,
    # Wallpaper Engine config.json to put back at the end instead of the copy taken at the start
    # (Wallpaper Engine rewrites its monitor map whenever it starts).
    [string] $WeConfigReference,
    # Abort when an engine's audio session peak exceeds this (0..1).
    [double] $AudioAbortPeak = 0.001
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
# `pwsh -File` passes `a,b` as one string: split lists here.
$Wallpapers = @($Wallpapers | ForEach-Object { $_ -split ',' } | ForEach-Object Trim | Where-Object { $_ })
$WeMonitors = @($WeMonitors | ForEach-Object { $_ -split ',' } | ForEach-Object Trim | Where-Object { $_ } | ForEach-Object { [int]$_ })
$LumenwallOutputs = @($LumenwallOutputs | ForEach-Object { $_ -split ',' } | ForEach-Object Trim | Where-Object { $_ })
$Engines = @($Engines | ForEach-Object { $_ -split ',' } | ForEach-Object Trim | Where-Object { $_ } | ForEach-Object ToLowerInvariant)
$LumenwallEnv = @($LumenwallEnv | ForEach-Object { $_ -split ',' } | ForEach-Object Trim | Where-Object { $_ })
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
Add-Type -Path (Join-Path $here 'BenchNative.cs')
$Native = [LumenwallBench.Native]

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$LogFile = Join-Path $OutDir 'bench.log'
function Log([string] $Message) {
    $line = '{0:HH:mm:ss.fff} {1}' -f (Get-Date), $Message
    Write-Host $line
    Add-Content -LiteralPath $LogFile -Value $line -Encoding utf8
}

$Cores = [Environment]::ProcessorCount
$Invariant = [Globalization.CultureInfo]::InvariantCulture
$WeExe = Join-Path $WeDir 'wallpaper64.exe'
$WeConfig = Join-Path $WeDir 'config.json'
$WeConfigDist = Join-Path $WeDir 'distribution\config.json'
$LwConfigDir = Join-Path $env:APPDATA 'Lumenwall'
$LwConfigFiles = @('config.toml', 'ui.json') | ForEach-Object { Join-Path $LwConfigDir $_ }
$UiNames = @('wallpaperui.exe', 'lumenwall-ui.exe')
$GpuEngineCat = [Diagnostics.PerformanceCounterCategory]::new('GPU Engine')
$GpuMemCat = [Diagnostics.PerformanceCounterCategory]::new('GPU Process Memory')

# ------------------------------------------------------------------------------------ helpers

function With([hashtable] $Table, [string] $Key, $Value) { $copy = $Table.Clone(); $copy[$Key] = $Value; $copy }

function SumOf($Items, [string] $Field) { $s = 0L; foreach ($i in $Items) { $s += $i.$Field }; $s }

function Fmt([double] $Value, [int] $Digits = 1) { $Value.ToString("F$Digits", $Invariant) }

function Get-Hash([string] $Path) {
    if (Test-Path -LiteralPath $Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash } else { $null }
}

function Get-EngineProcesses([string] $Name) {
    # Engine processes of this user, except the short-lived command-line clients this script starts.
    @($Native::Snapshot() | Where-Object { $_.Name -ieq $Name -and $_.ParentPid -ne $PID })
}

function Wait-Until([scriptblock] $Condition, [double] $TimeoutSeconds, [int] $StepMs = 200) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if (& $Condition) { return $true }
        Start-Sleep -Milliseconds $StepMs
    }
    return [bool](& $Condition)
}

# Runs a short-lived command and waits for it. Returns the exit code and output.
function Invoke-Exe([string] $Exe, [string[]] $Arguments, [hashtable] $Environment = @{}, [double] $TimeoutSeconds = 30) {
    $psi = [Diagnostics.ProcessStartInfo]::new($Exe)
    foreach ($a in $Arguments) { $psi.ArgumentList.Add($a) }
    foreach ($k in $Environment.Keys) { $psi.Environment[$k] = $Environment[$k] }
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $p = [Diagnostics.Process]::Start($psi)
    $out = $p.StandardOutput.ReadToEndAsync()
    $err = $p.StandardError.ReadToEndAsync()
    if (-not $p.WaitForExit([int]($TimeoutSeconds * 1000))) {
        return [pscustomobject]@{ ExitCode = $null; Output = 'timeout'; Pid = $p.Id }
    }
    $p.WaitForExit()
    [pscustomobject]@{ ExitCode = $p.ExitCode; Output = (($out.Result + $err.Result).Trim()); Pid = $p.Id }
}

function Invoke-WeControl([string[]] $Arguments) {
    $r = Invoke-Exe $WeExe (@('-control') + $Arguments) -TimeoutSeconds 20
    if ($r.ExitCode -ne 0) { Log "  WE -control $($Arguments -join ' '): exit $($r.ExitCode) $($r.Output)" }
    $r
}

# Lumenwall IPC (crates/lumenwall-ipc): one JSON object per line over a named pipe.
function Invoke-LwIpc([string] $Method, $Params = $null, [int] $TimeoutMs = 5000) {
    $pipe = [IO.Pipes.NamedPipeClientStream]::new('.', $IpcEndpoint, [IO.Pipes.PipeDirection]::InOut)
    try {
        $pipe.Connect($TimeoutMs)
        $writer = [IO.StreamWriter]::new($pipe, [Text.UTF8Encoding]::new($false))
        $writer.AutoFlush = $true
        $reader = [IO.StreamReader]::new($pipe, [Text.UTF8Encoding]::new($false))
        $send = {
            param($id, $method, $params)
            $frame = [ordered]@{ id = $id; method = $method }
            if ($null -ne $params) { $frame.params = $params }
            $writer.Write(($frame | ConvertTo-Json -Compress -Depth 8) + "`n")
            while ($true) {
                $task = $reader.ReadLineAsync()
                if (-not $task.Wait($TimeoutMs)) { throw "IPC $method`: no reply" }
                if ($null -eq $task.Result) { throw "IPC $method`: connection closed" }
                $msg = $task.Result | ConvertFrom-Json -Depth 20
                if ($msg.PSObject.Properties.Name -contains 'reply' -and $msg.reply.id -eq $id) { return $msg.reply }
            }
        }
        $null = & $send 1 'hello' @{ protocol = 1; client = 'bench-vs-we' }
        $reply = & $send 2 $Method $Params
        try { $writer.Write('{"id":3,"method":"bye"}' + "`n") } catch { }
        if ($reply.PSObject.Properties.Name -contains 'error') { throw "IPC $Method`: $($reply.error.code): $($reply.error.message)" }
        return $reply.ok
    }
    finally { $pipe.Dispose() }
}

function Test-LwIpc { try { $null = Invoke-LwIpc 'get_state' -TimeoutMs 300; $true } catch { $false } }

# Process tree of $RootPid: children by parent pid, a child must be younger than its parent
# (pid reuse). UI processes and their children go to Excluded.
function Get-Tree($Snapshot, [int] $RootPid) {
    $byParent = @{}
    foreach ($p in $Snapshot) {
        if (-not $byParent.ContainsKey($p.ParentPid)) { $byParent[$p.ParentPid] = [Collections.Generic.List[object]]::new() }
        $byParent[$p.ParentPid].Add($p)
    }
    $root = $Snapshot | Where-Object Pid -EQ $RootPid | Select-Object -First 1
    $included = [Collections.Generic.List[object]]::new()
    $excluded = [Collections.Generic.List[object]]::new()
    if (-not $root) { return @{ Included = $included; Excluded = $excluded } }
    $Native::Fill($root)
    $queue = [Collections.Generic.Queue[object]]::new()
    $queue.Enqueue(@($root, $false))
    while ($queue.Count -gt 0) {
        $item = $queue.Dequeue()
        $p = $item[0]; $ex = $item[1] -or ($UiNames -contains $p.Name.ToLowerInvariant())
        if ($ex) { $excluded.Add($p) } else { $included.Add($p) }
        if ($p.Pid -eq 0 -or -not $byParent.ContainsKey($p.Pid)) { continue }
        foreach ($c in $byParent[$p.Pid]) {
            if ($c.Pid -eq $p.Pid) { continue }
            $Native::Fill($c)
            if ($c.CreateTime -ne 0 -and $p.CreateTime -ne 0 -and $c.CreateTime -lt $p.CreateTime) { continue }
            $queue.Enqueue(@($c, $ex))
        }
    }
    @{ Included = $included; Excluded = $excluded }
}

# GPU Engine running time (100 ns) summed per "pid|luid|engtype".
function Read-GpuEngine {
    $sum = [Collections.Generic.Dictionary[string, long]]::new()
    foreach ($v in $GpuEngineCat.ReadCategory()['running time'].Values) {
        $n = $v.InstanceName
        $i = $n.IndexOf('_luid_'); $j = $n.IndexOf('_phys_'); $k = $n.LastIndexOf('_engtype_')
        if ($i -lt 4 -or $j -lt 0 -or $k -lt 0) { continue }
        $key = $n.Substring(4, $i - 4) + '|' + $n.Substring($i + 6, $j - $i - 6).ToUpperInvariant().Replace('0X', '0x') + '|' + $n.Substring($k + 9)
        $old = 0L
        $null = $sum.TryGetValue($key, [ref] $old)
        $sum[$key] = $old + $v.RawValue
    }
    $sum
}

# GPU Process Memory per "pid|luid": dedicated and shared bytes.
function Read-GpuMemory {
    $data = $GpuMemCat.ReadCategory()
    $res = [Collections.Generic.Dictionary[string, long[]]]::new()
    foreach ($pair in @(@('dedicated usage', 0), @('shared usage', 1))) {
        foreach ($v in $data[$pair[0]].Values) {
            $n = $v.InstanceName
            $i = $n.IndexOf('_luid_'); $j = $n.IndexOf('_phys_')
            if ($i -lt 4 -or $j -lt 0) { continue }
            $key = $n.Substring(4, $i - 4) + '|' + $n.Substring($i + 6, $j - $i - 6).ToUpperInvariant().Replace('0X', '0x')
            if (-not $res.ContainsKey($key)) { $res[$key] = [long[]]::new(2) }
            $res[$key][$pair[1]] += $v.RawValue
        }
    }
    $res
}

function Get-GpuSum($Gpu, [int[]] $Pids) {
    # "luid|engtype" -> running time summed over $Pids.
    $set = [Collections.Generic.HashSet[string]]::new(); foreach ($p in $Pids) { $null = $set.Add("$p") }
    $res = @{}
    foreach ($kv in $Gpu.GetEnumerator()) {
        $bar = $kv.Key.IndexOf('|')
        if (-not $set.Contains($kv.Key.Substring(0, $bar))) { continue }
        $rest = $kv.Key.Substring($bar + 1)
        $res[$rest] = [long]($res[$rest]) + $kv.Value
    }
    $res
}

function Get-MemSum($Mem, [int[]] $Pids) {
    # luid -> @(dedicated, shared) summed over $Pids.
    $set = [Collections.Generic.HashSet[string]]::new(); foreach ($p in $Pids) { $null = $set.Add("$p") }
    $res = @{}
    foreach ($kv in $Mem.GetEnumerator()) {
        $bar = $kv.Key.IndexOf('|')
        if (-not $set.Contains($kv.Key.Substring(0, $bar))) { continue }
        $luid = $kv.Key.Substring($bar + 1)
        if (-not $res.ContainsKey($luid)) { $res[$luid] = [long[]]::new(2) }
        $res[$luid][0] += $kv.Value[0]; $res[$luid][1] += $kv.Value[1]
    }
    $res
}

# One reading of everything; the difference of two readings gives a sample.
function Read-State([int] $EngineRoot, [int] $OtherRoot) {
    $snap = $Native::Snapshot()
    $tree = if ($EngineRoot) { Get-Tree $snap $EngineRoot } else { @{ Included = @(); Excluded = @() } }
    $other = if ($OtherRoot) { Get-Tree $snap $OtherRoot } else { @{ Included = @(); Excluded = @() } }
    [pscustomobject]@{
        Time     = [DateTime]::Now
        Ticks    = [Diagnostics.Stopwatch]::GetTimestamp()
        Tree     = $tree
        Other    = $other
        Gpu      = Read-GpuEngine
        Mem      = Read-GpuMemory
        Sys      = $Native::SystemTimes()
        Nv       = $Native::NvmlRead()
    }
}

function Get-CpuDelta($Prev, $Cur, [string] $Which) {
    # CPU time used by the processes of $Cur.$Which over the interval, in 100 ns.
    $before = @{}
    foreach ($p in $Prev.$Which.Included) { $before["$($p.Pid):$($p.CreateTime)"] = $p.CpuTime }
    $prevFileTime = $Prev.Time.ToFileTimeUtc()
    $total = 0L
    foreach ($p in $Cur.$Which.Included) {
        $key = "$($p.Pid):$($p.CreateTime)"
        if ($before.ContainsKey($key)) { $total += [Math]::Max(0, $p.CpuTime - $before[$key]) }
        elseif ($p.CreateTime -ge $prevFileTime) { $total += $p.CpuTime }
    }
    $total
}

$AdapterNames = @{}
function Get-AdapterTag([string] $Luid) {
    if ($AdapterNames.ContainsKey($Luid)) { return $AdapterNames[$Luid] }
    return $Luid
}

# A sample between two readings: one row of samples.csv. $Which: 'Tree' (the engine under
# test) or 'Other' (the other resident engine, for the resident idle baseline).
function New-Sample($Prev, $Cur, [hashtable] $Context, [string] $Which = 'Tree') {
    $opposite = if ($Which -eq 'Tree') { 'Other' } else { 'Tree' }
    $wall = ($Cur.Ticks - $Prev.Ticks) / [Diagnostics.Stopwatch]::Frequency
    $wall100ns = $wall * 1e7
    $pids = [int[]]@($Cur.$Which.Included | ForEach-Object Pid)
    $gpuNow = Get-GpuSum $Cur.Gpu $pids
    $gpuBefore = Get-GpuSum $Prev.Gpu $pids
    $mem = Get-MemSum $Cur.Mem $pids
    $sysIdle = $Cur.Sys[0] - $Prev.Sys[0]; $sysTotal = ($Cur.Sys[1] - $Prev.Sys[1]) + ($Cur.Sys[2] - $Prev.Sys[2])
    $row = [ordered]@{
        run          = $Context.Run
        phase        = $Context.Phase
        wallpaper    = $Context.Wallpaper
        engine       = $Context.Engine
        t_s          = Fmt (($Cur.Ticks - $Context.Start) / [Diagnostics.Stopwatch]::Frequency) 2
        time         = $Cur.Time.ToString('yyyy-MM-ddTHH:mm:ss.fff', $Invariant)
        processes    = $pids.Count
        cpu_pct      = Fmt ((Get-CpuDelta $Prev $Cur $Which) / $wall100ns * 100) 2
        private_mib  = Fmt ((SumOf $Cur.$Which.Included PrivateBytes) / 1MB) 1
        ws_mib       = Fmt ((SumOf $Cur.$Which.Included WorkingSet) / 1MB) 1
        ui_cpu_pct   = $null
        other_cpu_pct = Fmt ((Get-CpuDelta $Prev $Cur $opposite) / $wall100ns * 100) 2
        system_cpu_pct = Fmt ((1 - $sysIdle / [Math]::Max(1, $sysTotal)) * 100) 2
        covering     = $Context.Covering
        audio_peak   = $Context.AudioPeak
    }
    # NVIDIA GPU through NVML (what nvidia-smi reports): the whole board, every process.
    $nvNames = @('nv_power_w', 'nv_util_pct', 'nv_dec_pct', 'nv_clock_mhz', 'nv_memclock_mhz', 'nv_mem_mib', 'nv_pstate')
    for ($q = 0; $q -lt $nvNames.Count; $q++) {
        $row[$nvNames[$q]] = if ($Cur.Nv -and -not [double]::IsNaN($Cur.Nv[$q])) { Fmt $Cur.Nv[$q] 1 } else { $null }
    }
    # UI processes (excluded from the totals) for reference.
    $uiBefore = @{}; foreach ($p in $Prev.$Which.Excluded) { $uiBefore["$($p.Pid):$($p.CreateTime)"] = $p.CpuTime }
    $ui = 0L; foreach ($p in $Cur.$Which.Excluded) { $k = "$($p.Pid):$($p.CreateTime)"; if ($uiBefore.ContainsKey($k)) { $ui += [Math]::Max(0, $p.CpuTime - $uiBefore[$k]) } }
    $row.ui_cpu_pct = Fmt ($ui / $wall100ns * 100) 2
    foreach ($tag in $script:AdapterTags) {
        $luid = $script:AdapterLuids[$tag]
        $e3d = 0L; $vdec = 0L; $copy = 0L; $all = 0L
        foreach ($kv in $gpuNow.GetEnumerator()) {
            $parts = $kv.Key.Split('|')
            if ($parts[0] -ne $luid) { continue }
            $d = [Math]::Max(0, $kv.Value - [long]($gpuBefore[$kv.Key]))
            $all += $d
            switch -Regex ($parts[1]) { '^3D$' { $e3d += $d } '^VideoDecode' { $vdec += $d } '^Copy$' { $copy += $d } }
        }
        $row["gpu3d_pct_$tag"] = Fmt ($e3d / $wall100ns * 100) 2
        $row["gpu_vdec_pct_$tag"] = Fmt ($vdec / $wall100ns * 100) 2
        $row["gpu_copy_pct_$tag"] = Fmt ($copy / $wall100ns * 100) 2
        $row["gpu_all_pct_$tag"] = Fmt ($all / $wall100ns * 100) 2
        $m = $mem[$luid]
        $row["vram_ded_mib_$tag"] = Fmt ($(if ($m) { $m[0] } else { 0 }) / 1MB) 1
        $row["vram_shared_mib_$tag"] = Fmt ($(if ($m) { $m[1] } else { 0 }) / 1MB) 1
    }
    [pscustomobject]$row
}

# Per-process view over a whole window (first and last reading): processes.csv.
function Get-ProcessRows($First, $Last, [hashtable] $Context, [string] $Which = 'Tree') {
    $wall100ns = ($Last.Ticks - $First.Ticks) / [Diagnostics.Stopwatch]::Frequency * 1e7
    $before = @{}; foreach ($p in @($First.$Which.Included) + @($First.$Which.Excluded)) { $before["$($p.Pid):$($p.CreateTime)"] = $p.CpuTime }
    $excludedPids = @($Last.$Which.Excluded | ForEach-Object Pid)
    foreach ($p in @($Last.$Which.Included) + @($Last.$Which.Excluded)) {
        $k = "$($p.Pid):$($p.CreateTime)"
        $cpu = if ($before.ContainsKey($k)) { $p.CpuTime - $before[$k] } else { $p.CpuTime }
        $g1 = Get-GpuSum $Last.Gpu @($p.Pid); $g0 = Get-GpuSum $First.Gpu @($p.Pid)
        $m = Get-MemSum $Last.Mem @($p.Pid)
        $row = [ordered]@{
            run = $Context.Run; wallpaper = $Context.Wallpaper; engine = $Context.Engine
            pid = $p.Pid; parent = $p.ParentPid; name = $p.Name; excluded_ui = ($excludedPids -contains $p.Pid)
            cpu_pct = Fmt ($cpu / $wall100ns * 100) 2
            private_mib = Fmt ($p.PrivateBytes / 1MB) 1; ws_mib = Fmt ($p.WorkingSet / 1MB) 1
        }
        foreach ($tag in $script:AdapterTags) {
            $luid = $script:AdapterLuids[$tag]
            $d = 0L; foreach ($kv in $g1.GetEnumerator()) { if ($kv.Key.StartsWith("$luid|") -and $kv.Key.EndsWith('|3D')) { $d += $kv.Value - [long]($g0[$kv.Key]) } }
            $row["gpu3d_pct_$tag"] = Fmt ($d / $wall100ns * 100) 2
            $row["vram_ded_mib_$tag"] = Fmt ($(if ($m[$luid]) { $m[$luid][0] } else { 0 }) / 1MB) 1
        }
        [pscustomobject]$row
    }
}

function Get-CoveringWindows {
    $ignore = [int[]]@(Get-Process -Name explorer, wallpaper64, lumenwall, wallpaperui -ErrorAction SilentlyContinue | ForEach-Object Id)
    @($Native::CoveringWindows($ignore))
}

# ------------------------------------------------------------------------------------ engines

$script:WeRoot = 0
$script:LwRoot = 0
$script:LwProcess = $null
$script:WeStartedByUs = $false
$script:Notes = [Collections.Generic.List[string]]::new()
function Note([string] $Text) { Log "NOTE: $Text"; $script:Notes.Add($Text) }

function Close-WallpaperEngine([string] $When) {
    $procs = @(Get-EngineProcesses 'wallpaper64.exe') + @(Get-EngineProcesses 'wallpaperui.exe')
    if ($procs.Count -eq 0) { return }
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $rc = $Native::RestartManagerClose([int[]]@($procs | ForEach-Object Pid))
    $gone = Wait-Until { @(Get-EngineProcesses 'wallpaper64.exe').Count -eq 0 } 20
    if ($gone -and $rc -eq 0) {
        Log "WE closed by the Restart Manager ($When) in $($sw.ElapsedMilliseconds) ms"
    }
    else {
        Note "WE did not close through the Restart Manager ($When, code $rc): forced"
        Get-EngineProcesses 'wallpaper64.exe' | ForEach-Object { Stop-Process -Id $_.Pid -Force -ErrorAction SilentlyContinue }
        $null = Wait-Until { @(Get-EngineProcesses 'wallpaper64.exe').Count -eq 0 } 10
    }
    Get-EngineProcesses 'wallpaperui.exe' | ForEach-Object { Stop-Process -Id $_.Pid -Force -ErrorAction SilentlyContinue }
    $script:WeRoot = 0
}

function Start-WallpaperEngine {
    # Through Explorer, like a normal launch: not a child of this script.
    Start-Process -FilePath 'explorer.exe' -ArgumentList "`"$WeExe`""
    $script:WeStartedByUs = $true
    if (-not (Wait-Until { @(Get-EngineProcesses 'wallpaper64.exe').Count -ge 1 } 30)) { throw 'Wallpaper Engine did not start' }
    Start-Sleep -Seconds 3
    $procs = @(Get-EngineProcesses 'wallpaper64.exe')
    if ($procs.Count -ne 1) { throw "expected one wallpaper64.exe, found $($procs.Count)" }
    $script:WeRoot = $procs[0].Pid
    $windows = $Native::WindowsOf([int[]]@($script:WeRoot))
    if ($windows.Count) { Note "WE windows at start-up: $($windows -join ' / ')" }
    # Mute before anything is opened, and remove whatever the configuration restores.
    $null = Invoke-WeControl @('mute')
    $null = Invoke-WeControl @('closeWallpaper')
    Log "WE resident: pid $($script:WeRoot), muted, no wallpaper; $(Get-WeMonitorMap)"
}

function Resolve-WeRoot {
    $procs = @(Get-EngineProcesses 'wallpaper64.exe')
    if ($procs.Count -eq 1) { $script:WeRoot = $procs[0].Pid; return $true }
    return $false
}

# Lumenwall command line for a wallpaper: every monitor, or only -LumenwallOutputs.
function Get-LwArgs([string] $Dir) {
    $a = @('--no-config', '--silent', '--fps', "$script:TargetFps")
    if ($LumenwallOutputs.Count) { foreach ($o in $LumenwallOutputs) { $a += @('-r', $o, '--bg', $Dir) } }
    else { $a += $Dir }
    $a
}

# Which display each WE monitor index is, from the monitor map WE writes at start-up.
function Get-WeMonitorMap {
    try {
        $map = (Get-Content -LiteralPath $WeConfig -Raw | ConvertFrom-Json -AsHashtable)[$WeUser].general.user.monitormap
        if (-not $map) { return 'not written yet' }
        $displays = @($Native::DisplayDevices())
        $items = foreach ($k in $map.Keys) {
            $pnp = ($k -split '#')[1]
            $disp = @($displays | Where-Object { ($_ -split '\|')[2] -match "\\$pnp\\" } | ForEach-Object { ($_ -split '\|')[0] })
            "monitor $($map[$k].location) = $pnp = $($disp -join ',')"
        }
        ($items | Sort-Object) -join '; '
    }
    catch { "unknown ($_)" }
}

# Environment of the bench daemon and of the client commands that reach it.
function Get-LwEnv {
    $e = @{ LUMENWALL_IPC_ENDPOINT = $IpcEndpoint }
    if ($LumenwallLocalAppData) { $e.LOCALAPPDATA = $LumenwallLocalAppData }
    foreach ($kv in $LumenwallEnv) {
        $k, $v = "$kv" -split '=', 2
        if ($k) { $e[$k.Trim()] = "$v" }
    }
    $e
}

function Start-LumenwallDaemon([string] $FirstWallpaper) {
    $psi = [Diagnostics.ProcessStartInfo]::new($LumenwallExe)
    foreach ($a in (Get-LwArgs $FirstWallpaper)) { $psi.ArgumentList.Add($a) }
    $lwEnv = Get-LwEnv
    foreach ($k in $lwEnv.Keys) { $psi.Environment[$k] = $lwEnv[$k] }
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $script:LwProcess = [Diagnostics.Process]::Start($psi)
    $script:LwRoot = $script:LwProcess.Id
    if (-not (Wait-Until { Test-LwIpc } 30 250)) { throw 'the Lumenwall bench daemon did not open its IPC endpoint' }
    $null = Invoke-LwIpc 'clear' @{}
    $state = Invoke-LwIpc 'get_state'
    if (-not $state.muted) { throw 'the Lumenwall bench daemon is not muted' }
    Log "Lumenwall resident: pid $($script:LwRoot), muted, endpoint $IpcEndpoint, fps $script:TargetFps"
}

function Stop-LumenwallDaemon {
    if (-not $script:LwProcess -or $script:LwProcess.HasExited) { return }
    # IPC shutdown first: with every output cleared the daemon may have no window for WM_CLOSE.
    try { $null = Invoke-LwIpc 'shutdown' } catch { }
    if (-not $script:LwProcess.WaitForExit(10000)) {
        $null = & taskkill.exe /PID $script:LwRoot 2>&1
        if (-not $script:LwProcess.WaitForExit(5000)) {
            Note 'the Lumenwall bench daemon ignored shutdown and WM_CLOSE: forced'
            $script:LwProcess.Kill($true)
        }
    }
    Log "Lumenwall bench daemon stopped (exit $($script:LwProcess.ExitCode))"
    $script:LwRoot = 0
}

# Hands the wallpaper to the engine; returns $null, or the error when the engine refused it.
function Open-Wallpaper([string] $Engine, $Wp) {
    if ($Engine -eq 'we') {
        $null = Invoke-WeControl @('mute')
        $errors = foreach ($m in $WeMonitors) {
            $r = Invoke-WeControl @('openWallpaper', '-file', $Wp.Project, '-monitor', "$m")
            if ($r.ExitCode -ne 0) { "monitor $m`: exit $($r.ExitCode) $($r.Output)" }
        }
        $null = Invoke-WeControl @('mute')
        if ($errors) { return ($errors -join '; ') }
        return $null
    }
    $r = Invoke-Exe $LumenwallExe (Get-LwArgs $Wp.Dir) (Get-LwEnv)
    if ($r.ExitCode -ne 0 -or $r.Output -notmatch 'sent to it') {
        # The refusal is logged by the client (stderr is often empty for a GUI build): say what the log says.
        $why = @($r.Output -split "`r?`n" | Where-Object { $_ -match ' ERROR ' } | ForEach-Object { $_ -replace '^\S+\s+ERROR\s+', '' })[0]
        if (-not $why) { $why = $r.Output }
        $logRoot = if ($LumenwallLocalAppData) { $LumenwallLocalAppData } else { $env:LOCALAPPDATA }
        $log = Join-Path $logRoot "lumenwall\logs\lumenwall.$((Get-Date).ToUniversalTime().ToString('yyyy-MM-dd')).log"
        if (-not $why -and (Test-Path -LiteralPath $log)) { $why = (Get-Content -LiteralPath $log -Tail 20 | Where-Object { $_ -match ' ERROR ' } | Select-Object -Last 1) -replace '^\S+\s+ERROR\s+', '' }
        return "exit $($r.ExitCode): $why"
    }
    return $null
}

function Close-Wallpaper([string] $Engine) {
    if ($Engine -eq 'we') { $null = Invoke-WeControl @('closeWallpaper') }
    else { try { $null = Invoke-LwIpc 'clear' @{} } catch { Note "lumenwall clear: $_" } }
}

function Get-EngineRoot([string] $Engine) { if ($Engine -eq 'we') { $script:WeRoot } else { $script:LwRoot } }
function Get-OtherRoot([string] $Engine) { if ($Engine -eq 'we') { $script:LwRoot } else { $script:WeRoot } }

function Get-AudioPeak {
    $pids = @()
    foreach ($root in @($script:WeRoot, $script:LwRoot)) {
        if ($root) { $pids += @((Get-Tree $Native::Snapshot() $root).Included | ForEach-Object Pid) }
    }
    if ($pids.Count -eq 0) { return 0 }
    $Native::SessionPeak([int[]]$pids)
}

# ------------------------------------------------------------------------------------ phases

$script:Samples = [Collections.Generic.List[object]]::new()
$script:ProcRows = [Collections.Generic.List[object]]::new()
$script:Runs = [Collections.Generic.List[object]]::new()
$script:MaxAudioPeak = 0.0

# Samples once per second for $Seconds; returns the first and last readings.
function Measure-Window([hashtable] $Context, [int] $Seconds, [int] $EngineRoot, [int] $OtherRoot) {
    $Context.Start = [Diagnostics.Stopwatch]::GetTimestamp()
    $first = Read-State $EngineRoot $OtherRoot
    $prev = $first
    for ($k = 1; $k -le $Seconds; $k++) {
        $target = $Context.Start + $k * [Diagnostics.Stopwatch]::Frequency
        $wait = ($target - [Diagnostics.Stopwatch]::GetTimestamp()) / [Diagnostics.Stopwatch]::Frequency
        if ($wait -gt 0) { Start-Sleep -Milliseconds ([int]($wait * 1000)) }
        $cur = Read-State $EngineRoot $OtherRoot
        $covering = @(Get-CoveringWindows)
        $peak = Get-AudioPeak
        if ($peak -gt $script:MaxAudioPeak) { $script:MaxAudioPeak = $peak }
        if ($peak -gt $AudioAbortPeak) {
            if ($script:WeRoot) { Close-Wallpaper 'we' }
            if ($script:LwRoot) { Close-Wallpaper 'lumenwall' }
            throw "an engine played sound (peak $peak): wallpapers removed, bench aborted"
        }
        $Context.Covering = ($covering | ForEach-Object { ($_ -split '\|')[2] }) -join ' / '
        $Context.AudioPeak = Fmt $peak 4
        if ($Context.Phase -eq 'idle-resident') {
            # Both engines resident and empty: one row each.
            $script:Samples.Add((New-Sample $prev $cur (With $Context 'Engine' 'we') 'Tree'))
            $script:Samples.Add((New-Sample $prev $cur (With $Context 'Engine' 'lumenwall') 'Other'))
        }
        else { $script:Samples.Add((New-Sample $prev $cur $Context)) }
        $prev = $cur
    }
    @{ First = $first; Last = $prev }
}

function Measure-Idle([string] $Phase) {
    Log "idle baseline '$Phase' ($IdleSeconds s)"
    $ctx = @{ Run = $Phase; Phase = $Phase; Wallpaper = ''; Engine = '' }
    if ($Phase -eq 'idle-resident') {
        $w = Measure-Window $ctx $IdleSeconds $script:WeRoot $script:LwRoot
        $script:ProcRows.AddRange([object[]]@(Get-ProcessRows $w.First $w.Last (With $ctx 'Engine' 'we') 'Tree'))
        $script:ProcRows.AddRange([object[]]@(Get-ProcessRows $w.First $w.Last (With $ctx 'Engine' 'lumenwall') 'Other'))
    }
    else { $w = Measure-Window $ctx $IdleSeconds 0 0 }
    $script:Runs.Add([pscustomobject]@{ Run = $Phase; Kind = 'idle'; Engine = ''; Wallpaper = ''; Start = $w.First.Time; End = $w.Last.Time; FirstGpuMs = $null; CommandMs = $null; Lumenwall = $null; Error = $null })
}

function Invoke-Run([int] $Rep, [int] $Index, [string] $Engine, $Wp) {
    $run = '{0}-{1:D2}-{2}-{3}' -f $Rep, $Index, $Wp.Id, $Engine
    Log "run $run ($($Wp.Type), '$($Wp.Title)')"
    if ($Engine -eq 'we' -and -not (Resolve-WeRoot)) { Note "WE was not running before $run`: restarted"; Start-WallpaperEngine }
    if ($Engine -eq 'lumenwall' -and ($script:LwProcess.HasExited)) { Note "the Lumenwall daemon had exited before $run`: restarted"; Start-LumenwallDaemon $script:PrimeWallpaper.Dir }
    $covering = @(Get-CoveringWindows)
    if ($covering.Count) { Note "$run`: covering windows at start: $($covering -join ' / ')" }

    $root = Get-EngineRoot $Engine
    $snap = $Native::Snapshot()
    $pids0 = [int[]]@((Get-Tree $snap $root).Included | ForEach-Object Pid)
    $gpu0 = Get-GpuSum (Read-GpuEngine) $pids0

    $t0 = [Diagnostics.Stopwatch]::StartNew()
    $refused = Open-Wallpaper $Engine $Wp
    $commandMs = $t0.ElapsedMilliseconds
    if ($refused) {
        # Nothing to measure: the engine shows nothing. Recorded with its error.
        Note "$run could not run: $refused"
        $now = [DateTime]::Now
        $script:Runs.Add([pscustomobject]@{ Run = $run; Kind = 'run'; Engine = $Engine; Wallpaper = $Wp.Id; Start = $now; End = $now; FirstGpuMs = $null; CommandMs = $commandMs; Lumenwall = $null; Error = $refused })
        Close-Wallpaper $Engine
        Start-Sleep -Seconds $SettleSeconds
        return
    }

    # First GPU activity of the tree (any engine, any adapter), polled every 50 ms.
    $firstGpuMs = $null
    $warmEnd = [Math]::Max($WarmupSeconds, 1) * 1000
    while ($t0.ElapsedMilliseconds -lt $warmEnd) {
        if ($null -eq $firstGpuMs) {
            $pids = [int[]]@((Get-Tree $Native::Snapshot() $root).Included | ForEach-Object Pid)
            $now = 0L; foreach ($kv in (Get-GpuSum (Read-GpuEngine) $pids).GetEnumerator()) { $now += $kv.Value - [long]($gpu0[$kv.Key]) }
            # 1 ms of GPU time (100 ns units) since the command.
            if ($now -ge 10000) { $firstGpuMs = $t0.ElapsedMilliseconds }
            Start-Sleep -Milliseconds 50
        }
        else { Start-Sleep -Milliseconds 200 }
        $peak = Get-AudioPeak
        if ($peak -gt $script:MaxAudioPeak) { $script:MaxAudioPeak = $peak }
        if ($peak -gt $AudioAbortPeak) { Close-Wallpaper $Engine; throw "$run played sound (peak $peak): wallpaper removed, bench aborted" }
    }

    $ctx = @{ Run = $run; Phase = 'measure'; Wallpaper = $Wp.Id; Engine = $Engine }
    $w = Measure-Window $ctx $MeasureSeconds $root (Get-OtherRoot $Engine)
    $script:ProcRows.AddRange([object[]]@(Get-ProcessRows $w.First $w.Last $ctx))

    $lw = $null
    if ($Engine -eq 'lumenwall') {
        # After the window: get_stats turns on Lumenwall's own frame counters for one second.
        try {
            $state = Invoke-LwIpc 'get_state'
            $stats = Invoke-LwIpc 'get_stats' -TimeoutMs 5000
            $lw = [pscustomobject]@{
                Status = ($state.outputs | ForEach-Object { "$($_.output)=$($_.status)" + $(if ($_.message) { " ($($_.message))" } else { '' }) }) -join '; '
                Fps = $stats.fps; FrameMs = $stats.frame_time_ms; GpuPct = $stats.gpu_percent
                Outputs = ($stats.outputs | ForEach-Object { "$($_.output)=$([Math]::Round($_.fps, 1)) fps" + $(if (-not $_.rendering) { ' (not rendering)' } else { '' }) }) -join '; '
            }
            Log "  lumenwall: $($lw.Status); $($lw.Outputs); CPU $([Math]::Round($lw.FrameMs, 2)) ms/frame"
        }
        catch { Note "lumenwall state/stats after $run`: $_" }
    }
    Close-Wallpaper $Engine
    $script:Runs.Add([pscustomobject]@{ Run = $run; Kind = 'run'; Engine = $Engine; Wallpaper = $Wp.Id; Start = $w.First.Time; End = $w.Last.Time; FirstGpuMs = $firstGpuMs; CommandMs = $commandMs; Lumenwall = $lw; Error = $null })
    Log ("  command {0} ms, first GPU activity {1}" -f $commandMs, $(if ($null -ne $firstGpuMs) { "$firstGpuMs ms" } else { 'not seen' }))
    Start-Sleep -Seconds $SettleSeconds
}

# ------------------------------------------------------------------------------------ main

$backupDir = Join-Path $OutDir 'backup'
New-Item -ItemType Directory -Force -Path $backupDir | Out-Null
$state = [ordered]@{}
$userDaemon = $false

# Checks before anything changes.
foreach ($f in @($LumenwallExe, $WeExe, $WeConfig)) { if (-not (Test-Path -LiteralPath $f)) { throw "missing: $f" } }
if (Get-Process -Name 'lumenwall-ui' -ErrorAction SilentlyContinue) { throw 'lumenwall-ui is running: close it first (it would restart the daemon)' }
$weCfg = Get-Content -LiteralPath $WeConfig -Raw | ConvertFrom-Json -AsHashtable
if (-not $weCfg.ContainsKey($WeUser)) { throw "no user '$WeUser' in $WeConfig (use -WeUser)" }
$weUserCfg = $weCfg[$WeUser].general.user
if (-not $weUserCfg.hasshownwelcomedialog) { throw 'Wallpaper Engine has not finished its welcome wizard' }
$script:TargetFps = if ($Fps -gt 0) { $Fps } else { [int]$weUserCfg.fps }

$wallpaperList = @(foreach ($w in $Wallpapers) {
    $dir = if (Test-Path -LiteralPath $w -PathType Container) { (Resolve-Path -LiteralPath $w).Path } else { Join-Path $WorkshopDir $w }
    $project = Join-Path $dir 'project.json'
    if (-not (Test-Path -LiteralPath $project)) { throw "no project.json in $dir" }
    $pj = Get-Content -LiteralPath $project -Raw | ConvertFrom-Json -AsHashtable
    [pscustomobject]@{ Id = (Split-Path -Leaf $dir); Dir = $dir; Project = $project; Type = "$($pj['type'])".ToLowerInvariant(); Title = "$($pj['title'])" }
})

$runCount = $Repetitions * $wallpaperList.Count * 2
$estimate = 3 * ($IdleSeconds + $SettleSeconds) + 25 + $runCount * ($WarmupSeconds + $MeasureSeconds + $SettleSeconds + 3)
if ($WarmLumenwall) { $estimate += $wallpaperList.Count * (6 + $SettleSeconds) }
Log "bench-vs-we: $($wallpaperList.Count) wallpapers x 2 engines x $Repetitions repetition(s), warm-up $WarmupSeconds s, measurement $MeasureSeconds s, fps $script:TargetFps; estimated $([Math]::Ceiling($estimate / 60)) min; output $OutDir"

# Machine and versions.
$adapters = $Native::Adapters()
$script:AdapterLuids = [ordered]@{}
foreach ($a in $adapters) {
    $tag = switch ($a.VendorId) { 0x10DE { 'nvidia' } 0x1002 { 'amd' } 0x8086 { 'intel' } 0x1414 { 'basic' } default { 'gpu' } }
    if ($script:AdapterLuids.Contains($tag)) { $tag = "$tag$($script:AdapterLuids.Count)" }
    if ($tag -ne 'basic') { $script:AdapterLuids[$tag] = $a.Luid }
    $AdapterNames[$a.Luid] = $tag
}
$script:AdapterTags = @($script:AdapterLuids.Keys)
$machine = [ordered]@{
    date        = (Get-Date).ToString('yyyy-MM-dd HH:mm', $Invariant)
    os          = (Get-CimInstance Win32_OperatingSystem | ForEach-Object { "$($_.Caption) $($_.Version)" })
    cpu         = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name.Trim()
    logical_cpus = $Cores
    adapters    = @($adapters | ForEach-Object { [ordered]@{ name = $_.Description; luid = $_.Luid; tag = $AdapterNames[$_.Luid]; vram_mib = [int]($_.DedicatedVideoMemory / 1MB); outputs = @($_.Outputs) } })
    # name|adapter that drives it|monitor|primary|mode (EnumDisplayDevices, EnumDisplaySettings)
    displays    = @($Native::DisplayDevices())
    video_controllers = @(Get-CimInstance Win32_VideoController | ForEach-Object { "$($_.Name): $($_.CurrentHorizontalResolution)x$($_.CurrentVerticalResolution) @ $($_.CurrentRefreshRate) Hz" })
    nvidia      = $(try { (& nvidia-smi --query-gpu=name,driver_version --format=csv,noheader) -join '; ' } catch { 'n/a' })
    lumenwall   = (Invoke-Exe $LumenwallExe @('--version')).Output
    lumenwall_exe = $LumenwallExe
    we_version  = $(try { (Get-Content (Join-Path $WeDir 'version.json') -Raw | ConvertFrom-Json).version } catch { 'n/a' })
    we_exe      = $WeExe
    we_settings = [ordered]@{}
    fps         = $script:TargetFps
    wallpapers  = @($wallpaperList | ForEach-Object { [ordered]@{ id = $_.Id; type = $_.Type; title = $_.Title } })
    parameters  = [ordered]@{ repetitions = $Repetitions; warmup_s = $WarmupSeconds; measure_s = $MeasureSeconds; idle_s = $IdleSeconds; settle_s = $SettleSeconds; we_monitors = $WeMonitors; lumenwall_outputs = $(if ($LumenwallOutputs.Count) { $LumenwallOutputs } else { @('all') }); lumenwall_env = $LumenwallEnv; engines = $Engines }
}
foreach ($k in @('fps', 'preset', 'msaa', 'postprocessing', 'reflection', 'shadows', 'volumetrics', 'resolution', 'playbackfocus', 'playbackmaximized', 'playbackfullscreen', 'playbackaudio', 'videoframework', 'videohardwareacceleration', 'processpriority', 'highprecisiontimer')) {
    if ($weUserCfg.ContainsKey($k)) { $machine.we_settings[$k] = $weUserCfg[$k] }
}
$machine | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $OutDir 'machine.json') -Encoding utf8

# The user's state.
if ($WeConfigReference) {
    if (-not (Test-Path -LiteralPath $WeConfigReference)) { throw "missing: $WeConfigReference" }
    $state.we_config = Get-Hash $WeConfigReference
    Copy-Item -LiteralPath $WeConfigReference -Destination (Join-Path $backupDir 'we-config.json')
}
else {
    $state.we_config = Get-Hash $WeConfig
    Copy-Item -LiteralPath $WeConfig -Destination (Join-Path $backupDir 'we-config.json')
}
if (Test-Path -LiteralPath $WeConfigDist) { $state.we_config_dist = Get-Hash $WeConfigDist; Copy-Item -LiteralPath $WeConfigDist -Destination (Join-Path $backupDir 'we-distribution-config.json') }
foreach ($f in $LwConfigFiles) { if (Test-Path -LiteralPath $f) { $state["lw_$(Split-Path -Leaf $f)"] = Get-Hash $f; Copy-Item -LiteralPath $f -Destination (Join-Path $backupDir "lumenwall-$(Split-Path -Leaf $f)") } }
$weWasRunning = @(Get-EngineProcesses 'wallpaper64.exe').Count -gt 0
$userLw = @(Get-EngineProcesses 'lumenwall.exe')
$userDaemon = $userLw.Count -gt 0
if (-not $UserLumenwallExe) {
    $running = @($userLw | ForEach-Object { (Get-Process -Id $_.Pid -ErrorAction SilentlyContinue).Path } | Where-Object { $_ })
    $UserLumenwallExe = if ($running.Count) { $running[0] } else { $LumenwallExe }
}
Log "the user's daemon to relaunch at the end: $UserLumenwallExe"
Log ("state: WE {0}, Lumenwall daemon {1}, hashes {2}" -f $(if ($weWasRunning) { 'running' } else { 'not running' }), $(if ($userDaemon) { "running (pid $($userLw.Pid -join ','))" } else { 'not running' }), (($state.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value.Substring(0, 8))" }) -join ' '))

try {
    # Nobody may cover the desktop: Wallpaper Engine pauses on any maximized window and Lumenwall
    # pauses the output a window covers, so either would skew the comparison.
    if (-not (Wait-Until { @(Get-CoveringWindows).Count -eq 0 } $CoverWaitSeconds 1000)) {
        $msg = "maximized or fullscreen windows still open after $CoverWaitSeconds s: $((Get-CoveringWindows) -join ' / ')"
        if ($AllowCoveringWindows) { Note $msg } else { throw $msg }
    }

    if ($userDaemon) {
        foreach ($p in $userLw) { $null = & taskkill.exe /PID $p.Pid 2>&1 }
        if (-not (Wait-Until { @(Get-EngineProcesses 'lumenwall.exe').Count -eq 0 } 15)) { throw 'the Lumenwall daemon did not stop on WM_CLOSE' }
        Log 'the user''s Lumenwall daemon stopped (WM_CLOSE)'
    }
    Close-WallpaperEngine 'start'

    Start-Sleep -Seconds $SettleSeconds
    Measure-Idle 'idle-none-start'

    Start-WallpaperEngine
    # The daemon needs a wallpaper to start: a scene (this build may lack video or web support).
    $script:PrimeWallpaper = @($wallpaperList | Where-Object Type -EQ 'scene')[0]
    if (-not $script:PrimeWallpaper) { $script:PrimeWallpaper = $wallpaperList[0] }
    if ($Engines -contains 'lumenwall') { Start-LumenwallDaemon $script:PrimeWallpaper.Dir }
    if ($WarmLumenwall -and $Engines -contains 'lumenwall') {
        foreach ($wp in $wallpaperList) {
            $refused = Open-Wallpaper 'lumenwall' $wp
            Log "cache warm-up on Lumenwall: $($wp.Id)$(if ($refused) { " refused: $refused" })"
            Start-Sleep -Seconds 5
            Close-Wallpaper 'lumenwall'
            Start-Sleep -Seconds $SettleSeconds
        }
    }
    Start-Sleep -Seconds 5
    Measure-Idle 'idle-resident'

    for ($rep = 1; $rep -le $Repetitions; $rep++) {
        for ($i = 0; $i -lt $wallpaperList.Count; $i++) {
            $order = if ((($i + $rep - 1) % 2) -eq 0) { @('we', 'lumenwall') } else { @('lumenwall', 'we') }
            $n = 0
            foreach ($engine in $order) {
                $n++
                if ($Engines -contains $engine) { Invoke-Run $rep ($i * 2 + $n) $engine $wallpaperList[$i] }
            }
        }
    }

    Stop-LumenwallDaemon
    if (-not $KeepWallpaperEngine) {
        Close-WallpaperEngine 'end'
        Start-Sleep -Seconds $SettleSeconds
        Measure-Idle 'idle-none-end'
    }
}
catch {
    Log "ERROR: $_
$($_.ScriptStackTrace)"
    throw
}
finally {
    Log 'restoring the user''s state'
    try { Stop-LumenwallDaemon } catch { Note "stopping the bench daemon: $_" }
    if (-not $KeepWallpaperEngine) { try { Close-WallpaperEngine 'restore' } catch { Note "closing WE: $_" } }

    $restored = [ordered]@{}
    $weRunning = @(Get-EngineProcesses 'wallpaper64.exe').Count -gt 0
    foreach ($pair in @(@($WeConfig, 'we-config.json', 'we_config'), @($WeConfigDist, 'we-distribution-config.json', 'we_config_dist'))) {
        if (-not $state.Contains($pair[2])) { continue }
        if ((Get-Hash $pair[0]) -ne $state[$pair[2]]) {
            if ($weRunning) { Note "$($pair[0]) changed but Wallpaper Engine still runs: not restored" }
            else { Copy-Item -LiteralPath (Join-Path $backupDir $pair[1]) -Destination $pair[0] -Force; Log "restored $($pair[0])" }
        }
        $restored[$pair[2]] = (Get-Hash $pair[0]) -eq $state[$pair[2]]
    }
    foreach ($f in $LwConfigFiles) {
        $key = "lw_$(Split-Path -Leaf $f)"
        if (-not $state.Contains($key)) { continue }
        if ((Get-Hash $f) -ne $state[$key]) { Copy-Item -LiteralPath (Join-Path $backupDir "lumenwall-$(Split-Path -Leaf $f)") -Destination $f -Force; Log "restored $f" }
        $restored[$key] = (Get-Hash $f) -eq $state[$key]
    }
    if ($KeepWallpaperEngine -and -not $weRunning -and $weWasRunning) { Start-Process -FilePath 'explorer.exe' -ArgumentList "`"$WeExe`"" }
    if (($userDaemon -or $RelaunchLumenwall) -and @(Get-EngineProcesses 'lumenwall.exe').Count -eq 0) {
        # Through Explorer, with no arguments and none of this script's environment.
        Start-Process -FilePath 'explorer.exe' -ArgumentList "`"$UserLumenwallExe`""
        $up = Wait-Until { @(Get-EngineProcesses 'lumenwall.exe').Count -gt 0 } 20
        Log "the user's Lumenwall daemon relaunched: $up (pid $((Get-EngineProcesses 'lumenwall.exe').Pid -join ','))"
        $restored.lumenwall_daemon = $up
    }
    $restored.we_closed = @(Get-EngineProcesses 'wallpaper64.exe').Count -eq 0
    $restored.max_audio_peak = $script:MaxAudioPeak
    $restored.notes = @($script:Notes)
    $restored | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $OutDir 'restore.json') -Encoding utf8
    Log "restore: $(($restored.GetEnumerator() | Where-Object Key -NE 'notes' | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ', ')"

    if ($script:Samples.Count) { $script:Samples | Export-Csv -LiteralPath (Join-Path $OutDir 'samples.csv') -NoTypeInformation -Encoding utf8 }
    if ($script:ProcRows.Count) { $script:ProcRows | Export-Csv -LiteralPath (Join-Path $OutDir 'processes.csv') -NoTypeInformation -Encoding utf8 }
    if ($script:Runs.Count) {
        $script:Runs | Select-Object Run, Kind, Engine, Wallpaper, @{ n = 'Start'; e = { $_.Start.ToString('yyyy-MM-ddTHH:mm:ss.fff', $Invariant) } }, @{ n = 'End'; e = { $_.End.ToString('yyyy-MM-ddTHH:mm:ss.fff', $Invariant) } }, FirstGpuMs, CommandMs,
            @{ n = 'LwStatus'; e = { if ($_.Lumenwall) { $_.Lumenwall.Status } } }, @{ n = 'LwFps'; e = { if ($_.Lumenwall -and $null -ne $_.Lumenwall.Fps) { Fmt $_.Lumenwall.Fps 2 } } },
            @{ n = 'LwOutputs'; e = { if ($_.Lumenwall) { $_.Lumenwall.Outputs } } }, @{ n = 'LwCpuMsPerFrame'; e = { if ($_.Lumenwall -and $null -ne $_.Lumenwall.FrameMs) { Fmt $_.Lumenwall.FrameMs 3 } } },
            @{ n = 'LwGpuPct'; e = { if ($_.Lumenwall -and $null -ne $_.Lumenwall.GpuPct) { Fmt $_.Lumenwall.GpuPct 2 } } }, Error |
            Export-Csv -LiteralPath (Join-Path $OutDir 'runs.csv') -NoTypeInformation -Encoding utf8
    }
}

& (Join-Path $here 'summarize.ps1') -OutDir $OutDir
Log "done: $OutDir"
