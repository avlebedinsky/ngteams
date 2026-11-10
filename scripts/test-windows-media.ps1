# Offline production-runtime gate in the authorized Windows VM console. No account, network or physical capture.
# Generate -Samples with scripts/make-media-samples.py (FFmpeg); the runtime must decode every generated format.
#Requires -Version 7.0
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Samples, [string]$Runtime, [switch]$Serve, [string]$RunDirectory)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
. "$PSScriptRoot/lib/windows-runner.ps1"
if (!$Runtime) { $Runtime = Join-Path $root 'target/windows-media-runtime' }
$deniedConsole = 'devices::native::tests::windows_sandbox_reach_refuses_a_process_outside_the_authorized_console'

function Invoke-MediaProcess([string]$Executable, [string[]]$Arguments, [string]$Name, [int]$Seconds) {
    $start = [Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.WorkingDirectory = $root
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($start)
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    try {
        $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
        while (!$process.WaitForExit(200)) {
            $free = (Get-PSDrive C).Free
            if ($free -lt 1536MB -or [DateTime]::UtcNow -ge $deadline) {
                throw "$Name exceeded its disk/deadline guard (free bytes: $free; UTC deadline: $deadline)."
            }
        }
        if ($process.ExitCode -ne 0) { throw "$Name failed ($($process.ExitCode)); evidence: $RunDirectory" }
    } finally {
        if (!$process.HasExited) { $process.Kill($true); $process.WaitForExit(10000) | Out-Null }
        # Preserve failed/guarded runs too, after closing the exact child tree and draining its inherited pipes.
        if ($stdout.Wait(10000)) { [IO.File]::WriteAllText((Join-Path $RunDirectory "$Name.out"), $stdout.Result) }
        if ($stderr.Wait(10000)) { [IO.File]::WriteAllText((Join-Path $RunDirectory "$Name.err"), $stderr.Result) }
        $process.Dispose()
    }
}

if ($Serve) {
    $env:NGTEAMS_WINDOWS_VM_SANDBOX = 'win11'
    $env:UI_SANDBOX_DIR = $RunDirectory
    # Remove SDK paths even if the developer has a global SDK install. Resolve only the packaged runtime + OS.
    $sdk = Get-NgteamsGStreamerRoot $root
    $env:PATH = ($env:PATH -split ';' | Where-Object { $_ -and $_ -notmatch '(?i)gstreamer' -and
        (!$sdk -or !$_.StartsWith($sdk, [StringComparison]::OrdinalIgnoreCase)) }) -join ';'
    Set-NgteamsMediaRuntime $root $Runtime
    Remove-Item Env:GST_PLUGIN_PATH, Env:GST_PLUGIN_SYSTEM_PATH, Env:GST_PLUGIN_SCANNER -ErrorAction SilentlyContinue
    $env:GST_REGISTRY_1_0 = Join-Path $RunDirectory 'registry.bin'
    Remove-Item Env:GST_REGISTRY -ErrorAction SilentlyContinue
    $inspect = Join-Path $Runtime 'bin/gst-inspect-1.0.exe'
    # First prove the standard bin/lib/libexec layout works on its own, as it must in a release. No SDK or explicit
    # plugin/scanner paths may rescue this scan. Detailed checks below also pin each plugin's actual loaded path.
    Remove-Item Env:GST_PLUGIN_SYSTEM_PATH_1_0, Env:GST_PLUGIN_PATH_1_0, Env:GST_PLUGIN_SCANNER_1_0 -ErrorAction SilentlyContinue
    # The cold registry scan includes every production plugin, with a larger offline first-scan bound.
    Invoke-MediaProcess $inspect @() 'scan' 180
    Set-NgteamsMediaRuntime $root $Runtime
    Invoke-MediaProcess $inspect @('-b') 'blacklist' 15
    if ((Get-Content (Join-Path $RunDirectory 'blacklist.out') -Raw) -notmatch 'Total count: 0') {
        throw 'A packaged plugin was blacklisted.'
    }
    $plugins = (Get-Content (Join-Path $Runtime 'MEDIA-MANIFEST.json') -Raw | ConvertFrom-Json).plugins
    foreach ($plugin in $plugins) {
        Invoke-MediaProcess $inspect @($plugin) "plugin-$plugin" 20
        $text = Get-Content (Join-Path $RunDirectory "plugin-$plugin.out") -Raw
        $expected = Join-Path $Runtime "lib/gstreamer-1.0/gst$plugin.dll"
        if ($text -notmatch '(?m)^\s*Filename\s+(.+)$' -or $Matches[1].Trim() -ine $expected) {
            throw "Plugin $plugin did not load from the packaged file."
        }
    }
    foreach ($factory in @('wasapi2src','wasapi2sink','mfvideosrc','playbin','appsrc','appsink','scaletempo','deinterlace',
        'avdec_h264','avdec_aac','avdec_h265','dav1ddec','vp8dec','vp9dec','opusdec','vorbisdec','theoradec')) {
        Invoke-MediaProcess $inspect @($factory) "factory-$factory" 20
    }
    $samplesManifest = Get-Content (Join-Path $Samples 'SYNTHETIC.json') -Raw | ConvertFrom-Json
    $launch = Join-Path $Runtime 'bin/gst-launch-1.0.exe'
    foreach ($file in $samplesManifest.files) {
        $sample = Join-Path $Samples $file
        $uri = ([Uri]::new($sample)).AbsoluteUri
        Invoke-MediaProcess $launch @('-m', 'playbin', "uri=$uri", 'video-sink=fakesink sync=false',
            'audio-sink=fakesink sync=false', 'text-sink=fakesink sync=false') "decode-$file" 25
        # playbin may reach EOS with audio alone when a video's decoder is missing (or the other way round).
        # Its structured missing-plugin message must fail the sample even when gst-launch returns success.
        $messages = (Get-Content (Join-Path $RunDirectory "decode-$file.out") -Raw) +
            (Get-Content (Join-Path $RunDirectory "decode-$file.err") -Raw)
        if ($messages -match 'missing-plugin|Missing element|No decoder available|missing a plug-in') {
            throw "Sample $file did not decode all of its streams."
        }
    }
    $exes = Get-Content (Join-Path $RunDirectory 'executables.json') -Raw | ConvertFrom-Json
    Invoke-MediaProcess $exes.teams_media @('devices::native','--skip',$deniedConsole,'--test-threads=1','--nocapture') 'devices' 90
    Invoke-MediaProcess $exes.teams_media @('video::effects::tests','--test-threads=1','--nocapture') 'effects' 720
    Invoke-MediaProcess $exes.player @('--test-threads=1','--nocapture') 'player' 180
    Invoke-MediaProcess $exes.player_stream @('--test-threads=1','--nocapture') 'player-stream' 240
    Set-Content (Join-Path $RunDirectory 'passed') 'production media runtime passed'
    exit 0
}

Push-Location $root
try {
    $Samples = (Resolve-Path -LiteralPath $Samples).Path
    $Runtime = (Resolve-Path -LiteralPath $Runtime).Path
    & "$PSScriptRoot/package-windows-media.ps1" -Output $Runtime
    $runner = Get-NgteamsRunner $root
    $RunDirectory = Join-Path $root ('target/windows-media-smoke-' + [Guid]::NewGuid().ToString('N'))
    & $runner prepare --directory $RunDirectory
    if ($LASTEXITCODE -ne 0) { throw 'Cannot prepare private media evidence directory.' }
    Set-NgteamsMediaEnvironment $root
    $executables = @{}
    foreach ($target in @('player','player_stream','teams_media')) {
        $selection = if ($target -eq 'teams_media') { @('--lib') } else { @('--test', $target) }
        # cargo rustc's test profile builds the actual test harness. Extra options affect only this final target,
        # keeping dependency caches/ABI/debugging intact; its final link omits the gigabyte-scale native PDB.
        $arguments = @('rustc','--profile','test','--locked','-p','teams-media') + $selection +
            @('--target-dir','target/lab','-j1','--message-format=json','--','-C','link-arg=/DEBUG:NONE')
        Invoke-MediaProcess (Get-Command cargo -CommandType Application | Select-Object -First 1).Source `
            $arguments "build-$target" 1800
        Get-Content (Join-Path $RunDirectory "build-$target.out") | ForEach-Object {
            $artifact = $_ | ConvertFrom-Json
            if ($artifact.reason -eq 'compiler-artifact' -and $artifact.executable -and $artifact.profile.test) {
                $executables[$artifact.target.name] = $artifact.executable
            }
        }
        if (!$executables.ContainsKey($target)) { throw "Missing built media test target: $target" }
    }
    foreach ($name in @('teams_media','player','player_stream')) {
        if (!$executables.ContainsKey($name)) { throw "Missing built media test target: $name" }
    }
    # The refusal contract requires no VM-console opt-in; running it in the opted-in console tests the opposite
    # precondition. Execute it here, and keep all enumeration/streams/effects tests in the scheduled console.
    $previousSandbox = $env:NGTEAMS_WINDOWS_VM_SANDBOX
    $previousDirectory = $env:UI_SANDBOX_DIR
    try {
        Remove-Item Env:NGTEAMS_WINDOWS_VM_SANDBOX, Env:UI_SANDBOX_DIR -ErrorAction SilentlyContinue
        Set-NgteamsMediaRuntime $root $Runtime
        Invoke-MediaProcess $executables.teams_media @('--exact',$deniedConsole,'--nocapture') 'outside-console' 30
    } finally {
        $env:NGTEAMS_WINDOWS_VM_SANDBOX = $previousSandbox
        $env:UI_SANDBOX_DIR = $previousDirectory
    }
    $executables | ConvertTo-Json | Set-Content (Join-Path $RunDirectory 'executables.json')
    $taskName = 'ngteams-media-synthetic-' + [Guid]::NewGuid().ToString('N')
    $command = '& ' + (ConvertTo-NgteamsLiteral $PSCommandPath) + ' -Serve -Samples ' +
        (ConvertTo-NgteamsLiteral $Samples) + ' -Runtime ' + (ConvertTo-NgteamsLiteral $Runtime) +
        ' -RunDirectory ' + (ConvertTo-NgteamsLiteral $RunDirectory) + ' *> ' +
        (ConvertTo-NgteamsLiteral (Join-Path $RunDirectory 'console.log'))
    $command = 'try { ' + $command + ' } catch { [IO.File]::WriteAllText(' +
        (ConvertTo-NgteamsLiteral (Join-Path $RunDirectory 'fatal.txt')) + ', ($_ | Out-String)); exit 1 }'
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
    $action = New-ScheduledTaskAction -Execute (Get-Command pwsh -CommandType Application | Select-Object -First 1).Source `
        -Argument "-NoProfile -NonInteractive -EncodedCommand $encoded" -WorkingDirectory $root
    $principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) `
        -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Seconds 1800)
    try {
        Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings | Out-Null
        Start-ScheduledTask -TaskName $taskName
        $deadline = [DateTime]::UtcNow.AddSeconds(1750)
        do {
            Start-Sleep -Milliseconds 500
            if (Test-Path -LiteralPath (Join-Path $RunDirectory 'passed')) { break }
            if ((Get-ScheduledTask -TaskName $taskName).State -ne 'Running' -or [DateTime]::UtcNow -ge $deadline) {
                $result = (Get-ScheduledTaskInfo -TaskName $taskName).LastTaskResult
                throw "Media smoke did not pass (task result: $result); evidence: $RunDirectory"
            }
        } while ($true)
        Write-Output "Production media smoke passed; evidence: $RunDirectory"
    } finally {
        Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    }
} finally { Pop-Location }
