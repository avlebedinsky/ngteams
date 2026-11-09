# Shared implementation for the two Windows runner entry points. No passwords or tokens are accepted.
function Get-NgteamsRunner([string]$Root) {
    $runner = Join-Path $Root 'target/runner/debug/teams-runner.exe'
    Push-Location $Root
    try {
        # The helper has no GUI and is never an install-marked client. Bound its build, including compiler children.
        $build = Start-Process (Get-Command cargo -CommandType Application).Source `
            -ArgumentList @('build', '--locked', '-p', 'teams-runner', '--target-dir', 'target/runner') `
            -NoNewWindow -PassThru
        if (!$build.WaitForExit(1800000)) { $build.Kill($true); throw 'Runner build deadline expired.' }
        if ($build.ExitCode -ne 0) { throw "Runner build failed ($($build.ExitCode))." }
    } finally { Pop-Location }
    return $runner
}

function Set-NgteamsMediaEnvironment([string]$Root) {
    $gst = @(
        $env:GSTREAMER_1_0_ROOT_MSVC_X86_64,
        [Environment]::GetEnvironmentVariable('GSTREAMER_1_0_ROOT_MSVC_X86_64', 'User'),
        (Join-Path $Root 'windows-deps/gstreamer'),
        (Join-Path $env:LOCALAPPDATA 'Programs/gstreamer/1.0/msvc_x86_64'),
        (Join-Path $env:ProgramFiles 'gstreamer/1.0/msvc_x86_64')
    ) | Where-Object { $_ -and (Test-Path -LiteralPath (Join-Path $_ 'bin/pkg-config.exe')) } | Select-Object -First 1
    if ($gst) {
        $env:PATH = (Join-Path $gst 'bin') + ';' + $env:PATH
        $env:PKG_CONFIG_PATH = (Join-Path $gst 'lib/pkgconfig') + $(if ($env:PKG_CONFIG_PATH) { ';' + $env:PKG_CONFIG_PATH })
    }
}

function Build-NgteamsLab([string]$Root, [int]$TimeoutSeconds) {
    $runner = Get-NgteamsRunner $Root
    $previousLab = $env:TEAMS_LAB_BUILD
    $previousInstall = $env:TEAMS_PERSONAL_INSTALL
    $previousPath = $env:PATH
    $previousPkg = $env:PKG_CONFIG_PATH
    Push-Location $Root
    try {
        $env:TEAMS_LAB_BUILD = '1'
        Set-NgteamsMediaEnvironment $Root
        Remove-Item Env:TEAMS_PERSONAL_INSTALL -ErrorAction SilentlyContinue
        & $runner build --cargo (Get-Command cargo -CommandType Application).Source --timeout $TimeoutSeconds
    } finally {
        $env:TEAMS_LAB_BUILD = $previousLab
        $env:TEAMS_PERSONAL_INSTALL = $previousInstall
        $env:PATH = $previousPath
        $env:PKG_CONFIG_PATH = $previousPkg
        Pop-Location
    }
}

function Test-NgteamsHolder([string]$Directory) {
    $path = Join-Path $Directory 'holder.lock'
    if (!(Test-Path -LiteralPath $path)) { return $false }
    try {
        $stream = [IO.File]::Open($path, 'Open', 'ReadWrite', 'None')
        $stream.Dispose()
        return $false
    } catch [IO.IOException] { return $true }
}

function ConvertTo-NgteamsLiteral([string]$Value) {
    return "'" + $Value.Replace("'", "''") + "'"
}

function Invoke-NgteamsRunner {
    param([string]$Root, [string]$Directory, [string]$ScopeName, [string]$Scope, [string]$App,
        [int]$TimeoutSeconds, [switch]$Stop, [switch]$Status, [switch]$Serve, [string]$Script)
    if (!$env:LOCALAPPDATA -or ![IO.Path]::IsPathFullyQualified($env:LOCALAPPDATA)) {
        throw 'LOCALAPPDATA must be absolute.'
    }
    # Same task name across worktrees, so a pending start also reserves this alias. Hash avoids task-name syntax.
    $hash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData(
        [Text.Encoding]::Unicode.GetBytes($Directory.ToUpperInvariant())))
    $taskName = 'ngteams-vm-' + $hash.Substring(0,24)
    $task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    $running = (Test-NgteamsHolder $Directory) -or ($task -and $task.State -eq 'Running')
    if ($Status) {
        Write-Output "running: $([bool]$running)"
        Write-Output "run: $Directory"
        return
    }
    if ($Stop) {
        if ($running) {
            [IO.File]::WriteAllText((Join-Path $Directory 'stop'), '')
            $deadline = [DateTime]::UtcNow.AddSeconds(30)
            do {
                Start-Sleep -Milliseconds 100
                $task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
                $running = (Test-NgteamsHolder $Directory) -or ($task -and $task.State -eq 'Running')
            } while ($running -and [DateTime]::UtcNow -lt $deadline)
            if ($running) { throw "Cleanup still running; retained state in $Directory" }
        }
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
        # A crash may leave a snapshot; never delete it until the kernel lock and scheduled action are both gone.
        if ($ScopeName -eq 'Account' -and (Test-Path -LiteralPath (Join-Path $Directory 'snapshot'))) {
            $guard = [IO.File]::Open((Join-Path $Directory 'holder.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
            try { Remove-Item -LiteralPath (Join-Path $Directory 'snapshot') -Recurse -Force }
            finally { $guard.Dispose() }
        }
        Write-Output "stopped; logs retained in $Directory"
        return
    }
    if (!$Scope -or !$App -or ![IO.Path]::IsPathFullyQualified($App) -or !(Test-Path -LiteralPath $App -PathType Leaf)) {
        throw 'An account/alias and an existing absolute app executable are required.'
    }
    if (!$Serve -and $running) { throw "Already running or starting: $Directory" }
    $runner = if ($Serve) { Join-Path $Root 'target/runner/debug/teams-runner.exe' } else { Get-NgteamsRunner $Root }
    & $runner prepare --directory $Directory
    if ($LASTEXITCODE -ne 0) { throw 'Cannot prepare private run directory.' }
    if ($Serve) {
        Set-NgteamsMediaEnvironment $Root
        $env:NGTEAMS_WINDOWS_VM_SANDBOX = 'win11'
        $env:UI_SANDBOX_DIR = $Directory
        $scopeFlag = if ($ScopeName -eq 'Alias') { '--alias' } else { '--account' }
        & $runner run --app $App --directory $Directory $scopeFlag $Scope --timeout $TimeoutSeconds *>> (Join-Path $Directory 'holder.log')
        exit $LASTEXITCODE
    }
    # InteractiveToken uses the already logged-in console; SSH's token cannot display a window or use Credential
    # Manager. Never log on another user, ask for a password or use the highest run level.
    $command = '& ' + (ConvertTo-NgteamsLiteral $Script) + ' -Serve -' + $ScopeName + ' ' +
        (ConvertTo-NgteamsLiteral $Scope) + ' -TimeoutSeconds ' + $TimeoutSeconds
    if ($ScopeName -eq 'Account') { $command += ' -App ' + (ConvertTo-NgteamsLiteral $App) }
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
    $action = New-ScheduledTaskAction -Execute (Get-Command pwsh -CommandType Application).Source `
        -Argument "-NoProfile -NonInteractive -EncodedCommand $encoded" -WorkingDirectory $Root
    $principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) `
        -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Seconds ($TimeoutSeconds + 300))
    Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName $taskName
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        Start-Sleep -Milliseconds 100
        if (Test-NgteamsHolder $Directory) { Write-Output "started; run: $Directory"; return }
        $task = Get-ScheduledTask -TaskName $taskName
    } while ($task.State -eq 'Running' -and [DateTime]::UtcNow -lt $deadline)
    throw "No active holder; inspect $Directory/holder.log and scheduled task $taskName"
}
