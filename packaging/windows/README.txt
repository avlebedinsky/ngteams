ngteams for Windows 11 x64

Keep this whole folder together. bin/ngteams.exe is the desktop app and
bin/ngteamsctl.exe is its command-line controller. The sign-in worker, media
plugins, DLLs and notices beside them are required.

Install or update (ordinary user, no administrator rights):
1. Extract the zip, then open PowerShell in the extracted ngteams folder.
2. Run: powershell -NoProfile -ExecutionPolicy Bypass -File .\Install.ps1
3. Open ngteams from Start and sign in with your Microsoft 365 work/school account.

Setup validates the shipped files and installs into
%LOCALAPPDATA%\Programs\ngteams\<version>. It creates a Start shortcut and offers
ngteams as a handler for MSTeams links in Settings > Apps > Default apps. Windows
keeps your current default until you change it. Setup never starts the app.
On first normal launch the app completes its notification/COM shell registration.
If Microsoft WebView2 Runtime is missing, setup downloads Microsoft's Evergreen
bootstrapper, verifies its Microsoft signature, and installs it for your user.
This step needs an internet connection. Managed computers may require IT to
install/repair WebView2. Windows SmartScreen may warn that this app is unsigned.

Portable use: run bin\ngteams.exe without moving individual files. WebView2
Runtime must already be installed. Normal startup creates its notification
shortcut and registration for the folder it runs from. Run Install.ps1 for the
meeting-link association and a permanent location.

To validate the files without installing or starting anything:
  powershell -NoProfile -ExecutionPolicy Bypass -File .\Install.ps1 -VerifyOnly
PACKAGE-MANIFEST.json records the release version, source commit and SHA-256 of
every shipped file. Check the zip against the release's SHA256SUMS before use.

To remove: close ngteams, run Unregister.ps1 from the installed version's folder,
then delete that program folder. Older versions can be deleted once closed.
Account data under %LOCALAPPDATA%\ngteams and Windows Credential Manager entries
are retained. Sign out/forget an account in the app to remove its saved sign-in.

Optional translation, correction and summaries use your installed Claude Code
or Codex CLI. They stay off until enabled. No CLI service is bundled.

Project and source: https://github.com/nickalie/ngteams
