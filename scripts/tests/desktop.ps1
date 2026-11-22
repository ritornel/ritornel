# Run on Windows with pwsh -File scripts/tests/desktop.ps1.
# The runner must reject Windows PowerShell before it starts a build.
$runner = Join-Path $PSScriptRoot '..\desktop.ps1'
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Resolve-Path $runner), [ref]$null, [ref]$null)
if ($ast.ScriptRequirements.RequiredPSVersion -lt [version]'7.0') {
    throw 'The demo requires PowerShell 7 to pass each process argument separately.'
}
. $runner
Build-Server
$data = Join-Path ([IO.Path]::GetTempPath()) ("Ritornel demo " + [Guid]::NewGuid())
try {
    $child = Start-Served $data
    try {
        if (-not (Test-Path (Join-Path $data 'server.json'))) {
            throw 'The server did not start in the chosen folder.'
        }
    } finally {
        Stop-Served $child
    }
} finally {
    if (Test-Path $data) { Remove-Item -Recurse -Force $data }
}
