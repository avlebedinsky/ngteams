# Crop/close only the verified child of a synthetic scene, never a full desktop.
#Requires -Version 7.0
param([ValidateSet('Capture','Close')][string]$Action,[int]$ChildPid,[string]$Executable,[string]$OutputFile)
$ErrorActionPreference='Stop'
$process=Get-Process -Id $ChildPid
if($process.Path -ine $Executable -or $process.SessionId -ne [Diagnostics.Process]::GetCurrentProcess().SessionId){
 throw 'Scene child identity/session mismatch.'
}
. "$PSScriptRoot/windows-synthetic-shot.ps1"
$deadline=[DateTime]::UtcNow.AddSeconds(12)
do {
 $process.Refresh()
 if($process.HasExited){throw 'Scene child exited before its window action.'}
 if($Action -eq 'Close'){
  if($process.MainWindowHandle -and $process.CloseMainWindow()){exit 0}
 }elseif($process.MainWindowTitle -and (Save-NgteamsSyntheticShot $process $process.MainWindowTitle $OutputFile)){
  exit 0
 }
 Start-Sleep -Milliseconds 50
}while([DateTime]::UtcNow -lt $deadline)
throw "Scene $Action exceeded its window/focus deadline."
