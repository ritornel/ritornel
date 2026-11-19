# Runs the GPUI desktop app (apps/desktop) on Windows against an
# invented library, never a real one. The server is not built from here: the Windows port of the
# server is not on main yet, so -Server names a release ritornel-server.exe
# built elsewhere, and -DataDir its invented data directory. It starts that
# server with no sound, no media keys and a port of its own, runs the app,
# and stops the server when the app quits. Other arguments go to the app:
#   .\scripts\desktop.ps1 -Server C:\...\ritornel-server.exe -DataDir C:\...\data --size 1400x900
#   .\scripts\desktop.ps1 ... --measure 25 --out report.json
param(
    [Parameter(Mandatory = $true)][string]$Server,
    [Parameter(Mandatory = $true)][string]$DataDir,
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$Rest
)
$ErrorActionPreference = 'Stop'
$here = Join-Path (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)) "apps\desktop"

Push-Location $here
try { cargo build --release --quiet } finally { Pop-Location }
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# The server stops when its standard input closes: when this script ends.
$info = New-Object System.Diagnostics.ProcessStartInfo
$info.FileName = $Server
$info.Arguments = "serve --owned --data-dir `"$DataDir`" --listen 127.0.0.1:0 --output null --log-level warn"
$info.UseShellExecute = $false
$info.RedirectStandardInput = $true
$info.RedirectStandardOutput = $true
$info.EnvironmentVariables['RITORNEL_TEST_NO_MEDIA_SESSION'] = '1'
$served = [System.Diagnostics.Process]::Start($info)
try {
    $line = $served.StandardOutput.ReadLine()
    if ($null -eq $line -or -not $line.StartsWith('ready')) {
        throw "the server did not start: $line"
    }
    & "$here\target\release\ritornel-gpui.exe" --data-dir $DataDir @Rest
} finally {
    $served.StandardInput.Close()
    if (-not $served.WaitForExit(5000)) { $served.Kill() }
}
