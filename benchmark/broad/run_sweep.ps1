# M12 Broad Sweep launcher - spawns a fully detached process that survives
# the launching terminal closing.
#
# Usage: powershell -File benchmark/broad/run_sweep.ps1
#
# Output: benchmark/broad/sweep_output.txt (stdout)
#         benchmark/broad/sweep_stderr.txt (stderr - progress messages)
# Outliers: benchmark/broad/report.jsonl (written at end by the test)

$ErrorActionPreference = 'Stop'

# Wipe old outputs so we can tell apart this run's data
Remove-Item -Path "benchmark\broad\sweep_output.txt" -ErrorAction SilentlyContinue
Remove-Item -Path "benchmark\broad\sweep_stderr.txt" -ErrorAction SilentlyContinue
Remove-Item -Path "benchmark\broad\report.jsonl"    -ErrorAction SilentlyContinue

# Spawn in a hidden, new window so it survives this terminal closing.
# Do NOT use -NoNewWindow (it ties the child to the parent console).
$argList = @("test","--release","--test","fuzz_diff","--","--include-ignored","--nocapture","broad_sweep")

$proc = Start-Process -FilePath "cargo" `
    -ArgumentList $argList `
    -WorkingDirectory "D:\language\mdrcel" `
    -RedirectStandardOutput "D:\language\mdrcel\benchmark\broad\sweep_output.txt" `
    -RedirectStandardError "D:\language\mdrcel\benchmark\broad\sweep_stderr.txt" `
    -WindowStyle Hidden `
    -PassThru

$proc.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::BelowNormal

# Record the PID so we can find it later
Set-Content -Path "benchmark\broad\sweep.pid" -Value $proc.Id

Write-Host ""
Write-Host "Sweep launched - PID $($proc.Id) (BelowNormal priority, hidden window)."
Write-Host "It will keep running after you close this terminal."
Write-Host ""
Write-Host "Monitor progress:    Get-Content benchmark\broad\sweep_stderr.txt -Tail 5"
Write-Host "Check still running: Get-Process -Id $($proc.Id) -ErrorAction SilentlyContinue"
Write-Host "Kill it:             Stop-Process -Id $($proc.Id)"
Write-Host ""
Write-Host "Expected runtime: 6-10 hours at BelowNormal priority."
