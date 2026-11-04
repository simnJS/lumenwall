<#
.SYNOPSIS
  Shared-memory bisection: runs the D3D11 backend once per variant (PROBE_VARIANT) and prints
  the shared GPU memory after each load stage. Creates a GPU device: only run it when the PC is
  free. Takes %LOCALAPPDATA%\lumenwall-dev\gpu.lock.
#>
param(
    [Parameter(Mandatory)][string]$Exe,
    [string]$Name = 'probe',
    [string[]]$Variants = @('', 'cb=default', 'cb=big', 'vb=shared', 'cb=big,vb=shared'),
    [string]$Wallpaper = '3463520581',
    [int]$Seconds = 8,
    [int]$Warmup = 4
)
$ErrorActionPreference = 'Stop'
$lockDir = Join-Path $env:LOCALAPPDATA 'lumenwall-dev'
$lock = Join-Path $lockDir 'gpu.lock'
if (Test-Path $lock) { throw "GPU lock held by: $(Get-Content $lock -Raw)" }
Set-Content -Path $lock -Value $Name
try {
    foreach ($v in $Variants) {
        $env:PROBE_VARIANT = $v
        "== variant '$v'"
        & $Exe run --backend d3d11 --wallpaper $Wallpaper --seconds $Seconds --warmup $Warmup |
            Where-Object { $_ -like 'STAGE *' -or $_ -like 'RESULT *' } |
            ForEach-Object { if ($_ -like 'RESULT *') { ($_ -split ' ' | Where-Object { $_ -match '^(shared_mib|dedicated_mib|private_mib|cpu_percent|gpu_3d_percent|late_frames)=' }) -join ' ' } else { $_ } }
        Start-Sleep -Seconds 3
    }
}
finally {
    Remove-Item $lock -ErrorAction SilentlyContinue
    Remove-Item Env:PROBE_VARIANT -ErrorAction SilentlyContinue
}
