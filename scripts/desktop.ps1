#requires -Version 7.0
# Checks, builds and runs the desktop app (apps/desktop), in GPUI, on
# Windows, as scripts/desktop.sh does on macOS and Linux. `run`, `tones` and
# `app` build the server too, from the main workspace.
#
#   pwsh -File .\scripts\desktop.ps1 check          check formatting, run clippy with
#                                        warnings denied, run the tests and
#                                        build the app, as `mise run
#                                        desktop:check` and CI do
#   pwsh -File .\scripts\desktop.ps1 run [args]     run it against an invented library
#                                        of 8,600 albums, never a real one:
#                                        made once in apps\desktop\.invented,
#                                        served silently on a port of its
#                                        own, stopped on quit (-DataDir runs
#                                        it on another data directory)
#   pwsh -File .\scripts\desktop.ps1 tones [args]   run it against a library that plays:
#                                        one invented album of four tones a
#                                        minute and a half long, made once
#                                        with the pinned ffmpeg in
#                                        apps\desktop\.tones, played into
#                                        nothing
#   pwsh -File .\scripts\desktop.ps1 app            build the app with the release
#                                        ritornel-server.exe beside it, as
#                                        the launcher looks for it, in
#                                        apps\desktop\target\release\bundle\Ritornel
#
# Other arguments go to the app:
#   pwsh -File .\scripts\desktop.ps1 run --size 1400x900
#   pwsh -File .\scripts\desktop.ps1 run --measure 25 --out report.json
param(
    [Parameter(Position = 0)][ValidateSet('check', 'run', 'tones', 'app')][string]$Command = 'run',
    [string]$DataDir,
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$Rest
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$app = Join-Path $root 'apps\desktop'
$server = Join-Path $root 'target\release\ritornel-server.exe'
$ctl = Join-Path $root 'target\release\ritornelctl.exe'
$gpui = Join-Path $app 'target\release\ritornel-gpui.exe'

# Stops the script when the program it just ran failed.
function Assert-Exit {
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

function Build-Server([string[]]$Packages = @('ritornel-server')) {
    $chosen = $Packages | ForEach-Object { '--package', $_ }
    cargo build --locked --release --quiet --manifest-path "$root\Cargo.toml" @chosen
    Assert-Exit
}

function Build-App {
    cargo build --locked --release --quiet --manifest-path "$app\Cargo.toml"
    Assert-Exit
}

# Starts a silent server on $Data, with no media keys and a port of its
# own, and waits for its ready line. It stops when its standard input
# closes, which Stop-Served does and which happens anyway when this script
# ends.
function Start-Served([string]$Data) {
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $server
    foreach ($arg in @('serve', '--owned', '--data-dir', $Data, '--listen', '127.0.0.1:0',
            '--output', 'null', '--log-level', 'warn')) {
        $info.ArgumentList.Add($arg)
    }
    $info.UseShellExecute = $false
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.EnvironmentVariables['RITORNEL_TEST_NO_MEDIA_SESSION'] = '1'
    $served = [System.Diagnostics.Process]::Start($info)
    $line = $served.StandardOutput.ReadLine()
    if ($null -eq $line -or -not $line.StartsWith('ready')) {
        throw "the server did not start: $line"
    }
    $served
}

function Stop-Served($Served) {
    $Served.StandardInput.Close()
    if (-not $Served.WaitForExit(5000)) { $Served.Kill() }
}

# Runs the app on $Data until it quits. A release build is a windowed
# program, which PowerShell waits for only when its output is piped.
function Invoke-App([string]$Data) {
    $served = Start-Served $Data
    try {
        & $gpui --data-dir $Data @Rest | Out-Host
    } finally {
        Stop-Served $served
    }
}

if ($MyInvocation.InvocationName -eq '.') { return }

switch ($Command) {
    'check' {
        # As scripts/desktop.sh check does. The quoted '--' reaches cargo
        # whatever PowerShell makes of a bare one.
        cargo fmt --manifest-path "$app\Cargo.toml" --check
        Assert-Exit
        cargo clippy --locked --manifest-path "$app\Cargo.toml" --all-targets '--' -D warnings
        Assert-Exit
        cargo test --locked --manifest-path "$app\Cargo.toml"
        Assert-Exit
        cargo build --locked --release --manifest-path "$app\Cargo.toml"
        Assert-Exit
    }
    'run' {
        Build-App
        Build-Server
        if (-not $DataDir) {
            $DataDir = Join-Path $app '.invented\data'
            if (-not (Test-Path (Join-Path $DataDir '.invented'))) {
                $inventedRoot = Join-Path $app '.invented\root'
                New-Item -ItemType Directory -Force $DataDir, $inventedRoot | Out-Null
                & $server invent --data-dir $DataDir --root $inventedRoot --albums 8600 --showcase
                Assert-Exit
                New-Item -ItemType File (Join-Path $DataDir '.invented') | Out-Null
            }
        }
        Invoke-App $DataDir
    }
    'tones' {
        # The invented library has no audio, so nothing on it plays.
        $data = Join-Path $app '.tones\data'
        $tonesRoot = Join-Path $app '.tones\root'
        $folder = Join-Path $tonesRoot 'The Test Tones\Long Tones'
        if (-not (Test-Path $folder)) {
            New-Item -ItemType Directory -Force $folder | Out-Null
            Push-Location $root
            try {
                foreach ($track in 1..4) {
                    $hz = 110 * ($track + 1)
                    mise exec -- ffmpeg -loglevel error -y `
                        -f lavfi -i "sine=frequency=${hz}:duration=90" `
                        -metadata artist='The Test Tones' -metadata album_artist='The Test Tones' `
                        -metadata album='Long Tones' -metadata title="Tone of $hz" `
                        -metadata track=$track -metadata date=2020 `
                        (Join-Path $folder "0$track Tone of $hz.flac")
                    Assert-Exit
                }
            } finally {
                Pop-Location
            }
        }
        Build-App
        Build-Server @('ritornel-server', 'ritornelctl')
        New-Item -ItemType Directory -Force $data | Out-Null
        if (-not (Test-Path (Join-Path $data '.scanned'))) {
            $served = Start-Served $data
            try {
                & $ctl library create --data-dir $data | Out-Null
                Assert-Exit
                & $ctl scan $tonesRoot --data-dir $data | Out-Null
                Assert-Exit
            } finally {
                Stop-Served $served
            }
            New-Item -ItemType File (Join-Path $data '.scanned') | Out-Null
        }
        Invoke-App $data
    }
    'app' {
        # The app and the server it starts, side by side, as the launcher
        # looks for them (`ritornel_client::launch`).
        Build-Server
        Build-App
        $bundle = Join-Path $app 'target\release\bundle\Ritornel'
        if (Test-Path $bundle) { Remove-Item -Recurse -Force $bundle }
        New-Item -ItemType Directory -Force $bundle | Out-Null
        Copy-Item $gpui (Join-Path $bundle 'Ritornel.exe')
        Copy-Item $server (Join-Path $bundle 'ritornel-server.exe')
        "built $bundle"
    }
}
