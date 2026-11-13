# Installs the Windows installer, checks what it put on the machine, starts
# the app, uninstalls it, and checks what is left. Fails on the first thing
# that is not as the documentation says.
#
#   pwsh ci/installer-test.ps1 -Installer "target\release\bundle\nsis\Remote Desk_0.1.0_x64-setup.exe"
#
# It installs for the current user only and removes the app again. Meant for
# a throwaway machine (CI); on your own PC it will replace an installed copy.
param(
    [Parameter(Mandatory = $true)][string]$Installer,
    [string]$ProductName = 'Remote Desk'
)
$ErrorActionPreference = 'Stop'

function Fail([string]$Message) {
    Write-Host "::error title=Installer test::$Message"
    throw $Message
}
function Note([string]$Title, [string]$Message) {
    Write-Host "::notice title=$Title::$Message"
}
function UninstallEntry {
    Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall' -ErrorAction SilentlyContinue |
        ForEach-Object { Get-ItemProperty $_.PSPath } |
        Where-Object { $_.DisplayName -eq $ProductName }
}
# Anywhere a program could be set to start by itself.
function AutostartTraces {
    $found = @()
    foreach ($key in 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run',
                     'HKCU:\Software\Microsoft\Windows\CurrentVersion\RunOnce',
                     'HKLM:\Software\Microsoft\Windows\CurrentVersion\Run',
                     'HKLM:\Software\Microsoft\Windows\CurrentVersion\RunOnce') {
        $values = Get-ItemProperty $key -ErrorAction SilentlyContinue
        if ($values) {
            $values.PSObject.Properties |
                Where-Object { $_.Name -notlike 'PS*' -and "$($_.Name) $($_.Value)" -match 'Remote Desk|rc-desktop' } |
                ForEach-Object { $found += "$key\$($_.Name)" }
        }
    }
    foreach ($folder in [Environment]::GetFolderPath('Startup'), [Environment]::GetFolderPath('CommonStartup')) {
        Get-ChildItem $folder -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match 'Remote Desk|rc-desktop' } |
            ForEach-Object { $found += $_.FullName }
    }
    Get-ScheduledTask -ErrorAction SilentlyContinue |
        Where-Object { "$($_.TaskName) $($_.Actions.Execute)" -match 'Remote Desk|rc-desktop' } |
        ForEach-Object { $found += "scheduled task $($_.TaskName)" }
    Get-CimInstance Win32_Service -ErrorAction SilentlyContinue |
        Where-Object { "$($_.Name) $($_.PathName)" -match 'Remote Desk|rc-desktop' } |
        ForEach-Object { $found += "service $($_.Name)" }
    $found
}
function Shortcuts {
    $places = [Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('Desktop')
    Get-ChildItem $places -Recurse -Filter '*.lnk' -ErrorAction SilentlyContinue |
        Where-Object { $_.BaseName -eq $ProductName }
}

$Installer = (Resolve-Path $Installer).Path
$hash = (Get-FileHash $Installer -Algorithm SHA256).Hash.ToLower()
$sizeMb = [math]::Round((Get-Item $Installer).Length / 1MB, 1)
Note 'Installer' "$(Split-Path $Installer -Leaf): $sizeMb MB, sha256 $hash"

if (UninstallEntry) { Fail 'the app is already installed here; this test needs a machine without it' }

# ---- install, silently, for this user --------------------------------------
$setup = Start-Process $Installer -ArgumentList '/S' -PassThru -Wait
if ($setup.ExitCode -ne 0) { Fail "the installer exited with code $($setup.ExitCode)" }

$entry = UninstallEntry
if (-not $entry) { Fail 'no uninstall entry for the current user after installing' }
$dir = $entry.InstallLocation.Trim('"')
if (-not (Test-Path $dir)) { Fail "the install folder $dir does not exist" }
if ($dir -notlike "$env:LOCALAPPDATA*") { Fail "installed to $dir, outside the user's own folder" }
if (Get-ItemProperty 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue |
        Where-Object { $_.DisplayName -eq $ProductName }) {
    Fail 'the installer registered the app for all users'
}

$files = Get-ChildItem $dir -Recurse -File
$app = $files | Where-Object { $_.Extension -eq '.exe' -and $_.Name -notmatch 'uninstall' } | Select-Object -First 1
$uninstaller = $files | Where-Object { $_.Name -match '^uninstall.*\.exe$' } | Select-Object -First 1
if (-not $app) { Fail "no application in $dir" }
if (-not $uninstaller) { Fail "no uninstaller in $dir" }
Note 'Installed' "to $dir; files: $(($files | ForEach-Object { $_.Name }) -join ', '); version $($entry.DisplayVersion); publisher '$($entry.Publisher)'"

$autostart = @(AutostartTraces)
if ($autostart.Count -gt 0) { Fail "the installer set the app to start by itself: $($autostart -join '; ')" }
$shortcuts = @(Shortcuts)
Note 'Shortcuts' ("created: " + $(if ($shortcuts) { ($shortcuts | ForEach-Object { $_.FullName.Replace($env:USERPROFILE, '~') }) -join ', ' } else { 'none' }))

# ---- start it: it must run, and do nothing until asked ---------------------
$process = Start-Process $app.FullName -PassThru
Start-Sleep -Seconds 12
$process.Refresh()
if ($process.HasExited) { Fail "the app exited by itself with code $($process.ExitCode)" }
$listening = @(Get-NetTCPConnection -State Listen -OwningProcess $process.Id -ErrorAction SilentlyContinue)
if ($listening.Count -gt 0) {
    Fail "the app listens on port(s) $(($listening.LocalPort | Sort-Object -Unique) -join ', ') before anyone started sharing"
}
$autostart = @(AutostartTraces)
if ($autostart.Count -gt 0) { Fail "running the app set it to start by itself: $($autostart -join '; ')" }
Note 'Started' "the installed app ran for 12 s, listened on no port, and added nothing that starts it automatically"
Stop-Process -Id $process.Id -Force
$process.WaitForExit(10000) | Out-Null

# ---- uninstall, silently ---------------------------------------------------
Start-Process $uninstaller.FullName -ArgumentList '/S' -Wait
# The uninstaller hands over to a copy of itself; wait for that to finish.
$deadline = (Get-Date).AddSeconds(90)
while ((Test-Path $app.FullName) -or (UninstallEntry)) {
    if ((Get-Date) -gt $deadline) { Fail 'the app or its uninstall entry is still there 90 s after uninstalling' }
    Start-Sleep -Seconds 1
}
Start-Sleep -Seconds 2
$leftShortcuts = @(Shortcuts)
if ($leftShortcuts.Count -gt 0) { Fail "shortcuts left behind: $($leftShortcuts.FullName -join ', ')" }
$autostart = @(AutostartTraces)
if ($autostart.Count -gt 0) { Fail "left behind after uninstalling: $($autostart -join '; ')" }

# What stays is the user's own data, which the silent uninstall does not touch.
$data = Get-ChildItem $env:APPDATA, $env:LOCALAPPDATA -Directory -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match 'Remote ?Desk|remotedesk|rc-desktop' }
$left = if ($data) { ($data | ForEach-Object { $_.FullName.Replace($env:USERPROFILE, '~') }) -join ', ' } else { 'nothing' }
Note 'Uninstalled' "application, uninstall entry and shortcuts are gone; left in the user's profile: $left"
Write-Host 'installer test passed'
