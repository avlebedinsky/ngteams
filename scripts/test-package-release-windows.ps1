# Offline orchestration regression: stale rust-cache directories cannot poison subsequent release media builds.
#Requires -Version 7.0
$ErrorActionPreference = 'Stop'
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('ngteams-release-contract-' + [Guid]::NewGuid().ToString('N'))
New-Item (Join-Path $fixture 'scripts'), (Join-Path $fixture 'target/windows-media-release') -ItemType Directory -Force | Out-Null
try {
    Copy-Item "$PSScriptRoot/package-release-windows.ps1" "$fixture/scripts/"
    # Substitute only the expensive closure/notices/PE stages. The real wrapper chooses and cleans its paths.
    @'
param($Output, $Binary)
if (Test-Path $Output) { throw 'Release reused an existing media directory.' }
New-Item $Output -ItemType Directory | Out-Null
Set-Content (Join-Path $Output 'MEDIA-MANIFEST.json') '{}'
Add-Content (Join-Path (Split-Path $PSScriptRoot) 'runtimes.txt') $Output
'@ | Set-Content "$fixture/scripts/package-windows-media.ps1"
    @'
import pathlib, sys
if sys.argv[1] == 'stage':
    pathlib.Path(sys.argv[sys.argv.index('--output')+1]).mkdir(parents=True)
    pathlib.Path(sys.argv[sys.argv.index('--archive')+1]).write_text('synthetic archive')
else:
    pathlib.Path(sys.argv[-1]).mkdir(parents=True)
'@ | Set-Content "$fixture/scripts/windows-release.py"
    Copy-Item "$fixture/scripts/windows-release.py" "$fixture/scripts/windows-release-notices.py"
    foreach ($run in 1..2) {
        & "$fixture/scripts/package-release-windows.ps1" -Binaries "$fixture/binaries" -Version "0.1.0.$run" `
            -Commit ('a' * 40) -Output "$fixture/dist"
        # Model rust-cache's restored empty directory on the next invocation.
        if (!(Test-Path "$fixture/target/windows-media-release")) { throw 'Cache fixture lost.' }
        if (!(Test-Path "$fixture/dist/ngteams-0.1.0.$run-windows-x86_64.zip")) { throw 'Archive missing.' }
    }
    $runtimes = @(Get-Content "$fixture/runtimes.txt")
    if ($runtimes.Count -ne 2 -or $runtimes[0] -eq $runtimes[1]) { throw 'Each release must assemble fresh media.' }
    foreach ($runtime in $runtimes) {
        if ($runtime.StartsWith($fixture, [StringComparison]::OrdinalIgnoreCase)) { throw 'Runtime entered the cached checkout.' }
        if (Test-Path $runtime) { throw 'Owned temporary runtime was not cleaned up.' }
    }
    Write-Output 'Two releases passed with a restored empty target/windows-media-release directory.'
} finally {
    Remove-Item -LiteralPath $fixture -Recurse -Force
}
