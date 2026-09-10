# Copyright (C) 2026 leeymxz
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Install the latest PlayDL release on Windows.
#
#   powershell -ExecutionPolicy Bypass -File install.ps1          # GUI bundle (default)
#   powershell -ExecutionPolicy Bypass -File install.ps1 -Cli     # CLI only
#   powershell -ExecutionPolicy Bypass -File install.ps1 -Version v0.2.0
#   powershell -ExecutionPolicy Bypass -File install.ps1 -Beta    # newest -rc pre-release
#
# Or straight from the repo:
#   irm https://raw.githubusercontent.com/leeymxz/playdl/main/install.ps1 | iex
#
# Default install is the GUI bundle: playdl.exe, pdl.exe, playdl-gui.exe,
# playdl-host.exe, playdl-updater.exe plus browser extensions and native-host
# installer, into %LOCALAPPDATA%\Programs\PlayDL.
# -Cli installs only playdl.exe + pdl.exe.

param(
  [switch]$Cli,
  [switch]$Beta,
  [switch]$Desktop,
  [string]$Version = "",
  [string]$InstallDir = ""
)

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo = "leeymxz/playdl"
if (-not $InstallDir) { $InstallDir = Join-Path $env:LOCALAPPDATA "Programs\PlayDL" }

switch ($env:PROCESSOR_ARCHITECTURE) {
  "AMD64" { $Arch = "amd64" }
  "ARM64" { $Arch = "arm64" }
  default { throw "unsupported architecture: $env:PROCESSOR_ARCHITECTURE" }
}

function Get-CoreVersion([string]$Tag) {
  [version](($Tag.TrimStart("v") -split "[-+]")[0])
}

if (-not $Version) {
  if ($Beta) {
    $Releases = Invoke-RestMethod -UseBasicParsing `
      -Uri "https://api.github.com/repos/$Repo/releases?per_page=30"
    $Stable = ($Releases | Where-Object { -not $_.prerelease } | Select-Object -First 1).tag_name
    $Rc = ($Releases | Where-Object { $_.prerelease -or $_.tag_name -match "-rc" } |
      Select-Object -First 1).tag_name
    if (-not $Stable -and -not $Rc) { throw "could not resolve a release tag" }
    if ($Rc -and (-not $Stable -or ((Get-CoreVersion $Rc) -gt (Get-CoreVersion $Stable)))) {
      $Version = $Rc
    } else {
      $Version = $Stable
    }
  } else {
    $Version = (Invoke-RestMethod -UseBasicParsing `
      -Uri "https://api.github.com/repos/$Repo/releases/latest").tag_name
    if (-not $Version) { throw "could not resolve the latest release tag" }
  }
}
$Ver = $Version.TrimStart("v")

$Core = $Ver -replace '-.*$', ''
$AssetPrefix = if ($Cli) { "playdl-cli" } else { "playdl" }
$Candidates = if ($Core -ne $Ver) { @($Ver, $Core) } else { @($Ver) }
$DlBase = "https://github.com/$Repo/releases/download/$Version"

$Mode = if ($Cli) { "cli" } else { "gui" }
Write-Host "playdl $Version ($Mode) -> $InstallDir  [windows/$Arch]"

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("playdl-install-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Force -Path $Tmp | Out-Null
try {
  $Name = $null
  $Zip = $null
  foreach ($Cand in $Candidates) {
    $Try = "$AssetPrefix-$Cand-windows-$Arch"
    $Dest = Join-Path $Tmp "$Try.zip"
    Write-Host "downloading $DlBase/$Try.zip"
    try {
      Invoke-WebRequest -UseBasicParsing -Uri "$DlBase/$Try.zip" -OutFile $Dest
      $Name = $Try; $Zip = $Dest; break
    } catch {
      Remove-Item $Dest -Force -ErrorAction SilentlyContinue
      if ($Cand -eq $Candidates[-1]) { throw }
    }
  }
  Expand-Archive -Path $Zip -DestinationPath $Tmp
  $Src = Join-Path $Tmp $Name

  New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

  $Running = @(Get-Process -Name playdl-gui, playdl-host, playdl-updater -ErrorAction SilentlyContinue)
  if ($Running.Count -gt 0) {
    Write-Host "stopping the running PlayDL..."
    foreach ($p in $Running) { $null = $p.CloseMainWindow() }
    Start-Sleep -Milliseconds 1200
    $Running | Where-Object { -not $_.HasExited } | Stop-Process -Force -ErrorAction SilentlyContinue
  }

  Copy-Item (Join-Path $Src "playdl.exe") $InstallDir -Force
  Write-Host "installed $InstallDir\playdl.exe"

  # Install pdl.exe (short alias)
  $PdlExe = Join-Path $InstallDir "pdl.exe"
  if (Test-Path (Join-Path $Src "pdl.exe")) {
    Copy-Item (Join-Path $Src "pdl.exe") $InstallDir -Force
  } else {
    Copy-Item (Join-Path $Src "playdl.exe") $PdlExe -Force
  }
  Write-Host "installed $InstallDir\pdl.exe (short alias)"

  if (-not $Cli) {
    Copy-Item (Join-Path $Src "playdl-gui.exe")  $InstallDir -Force
    Copy-Item (Join-Path $Src "playdl-host.exe") $InstallDir -Force
    Write-Host "installed $InstallDir\playdl-gui.exe"
    Write-Host "installed $InstallDir\playdl-host.exe"
    $UpdaterExe = Join-Path $Src "playdl-updater.exe"
    if (Test-Path $UpdaterExe) {
      Copy-Item $UpdaterExe $InstallDir -Force
      Write-Host "installed $InstallDir\playdl-updater.exe"
    }

    foreach ($d in "extensions", "scripts") {
      $dest = Join-Path $InstallDir $d
      if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
      Copy-Item (Join-Path $Src $d) $InstallDir -Recurse -Force
    }
    Write-Host "installed $InstallDir\extensions (browser extensions)"

    $NativeHost = Join-Path $InstallDir "scripts\install-native-host.ps1"
    $HostExe = Join-Path $InstallDir "playdl-host.exe"
    $PsExe = (Get-Process -Id $PID).Path
    if (-not $PsExe) { $PsExe = "powershell" }
    $PrevEap = $ErrorActionPreference
    try {
      $ErrorActionPreference = "Continue"
      & $PsExe -NoProfile -ExecutionPolicy Bypass -File $NativeHost -NoBuild -HostBin $HostExe
      $Rc = $LASTEXITCODE
      $ErrorActionPreference = $PrevEap
      if ($Rc -ne 0) { throw "install-native-host.ps1 exited with $Rc" }
    } catch {
      $ErrorActionPreference = $PrevEap
      Write-Warning "browser integration was not registered: $($_.Exception.Message)"
      Write-Host "  retry with: powershell -NoProfile -ExecutionPolicy Bypass -File `"$NativeHost`" -NoBuild -HostBin `"$HostExe`""
    }

    $ExtDir = Join-Path $InstallDir "extensions"
    $Xpi = Get-ChildItem -Path (Join-Path $ExtDir "playdl-firefox-*.xpi") `
      -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName
    if (-not $Xpi) { $Xpi = Join-Path $ExtDir "firefox\manifest.json" }
    Write-Host ""
    Write-Host "Browser extension - the builds in $ExtDir are unsigned,"
    Write-Host "so each browser loads them through its developer mode:"
    Write-Host ""
    Write-Host "  Chrome / Edge / Brave / Opera / Vivaldi / Arc / Chromium"
    Write-Host "    1. open chrome://extensions (edge://extensions, brave://extensions, ...)"
    Write-Host "    2. turn on `"Developer mode`" (top right in Chrome; bottom left in Edge)"
    Write-Host "    3. click `"Load unpacked`" and select:"
    Write-Host "           $ExtDir\chrome"
    Write-Host ""
    Write-Host "  Firefox - temporary (every edition; removed at the next restart)"
    Write-Host "    1. open about:debugging#/runtime/this-firefox"
    Write-Host "    2. click `"Load Temporary Add-on...`" and select:"
    Write-Host "           $Xpi"
    Write-Host ""
    Write-Host "  Firefox - permanent (Developer Edition, Nightly and ESR only)"
    Write-Host "    1. in about:config set xpinstall.signatures.required = false"
    Write-Host "    2. in about:addons use the gear icon > `"Install Add-on From File...`""
    Write-Host "       and pick the .xpi in $ExtDir"
    Write-Host ""
    Write-Host "Restart the browser once so it picks up PlayDL's native-messaging manifest,"
    Write-Host "then check the PlayDL toolbar icon: the status dot turns green when the"
    Write-Host "extension has reached the app."
    if (Test-Path (Join-Path $ExtDir "INSTALL.txt")) {
      Write-Host "Full instructions: $ExtDir\INSTALL.txt"
    }

    $GuiExe = Join-Path $InstallDir "playdl-gui.exe"
    $Shell = New-Object -ComObject WScript.Shell
    function New-PlayDLShortcut([string]$LnkPath) {
      $Lnk = $Shell.CreateShortcut($LnkPath)
      $Lnk.TargetPath = $GuiExe
      $Lnk.WorkingDirectory = $InstallDir
      $Lnk.Description = "Multi-connection download accelerator"
      $Lnk.IconLocation = "$GuiExe,0"
      $Lnk.Save()
    }
    $StartMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
    New-PlayDLShortcut (Join-Path $StartMenu "PlayDL Download Manager.lnk")
    Write-Host "installed start-menu shortcut"
    if ($Desktop) {
      New-PlayDLShortcut (Join-Path ([Environment]::GetFolderPath("Desktop")) "PlayDL Download Manager.lnk")
      Write-Host "installed desktop shortcut"
    }

    $UninstallScript = Join-Path $InstallDir "scripts\uninstall.ps1"
    if (-not (Test-Path $UninstallScript)) {
      try {
        Invoke-WebRequest -UseBasicParsing `
          -Uri "https://raw.githubusercontent.com/$Repo/$Version/uninstall.ps1" `
          -OutFile $UninstallScript
      } catch {
        $UninstallScript = $null
      }
    }
    $UninstallCmd = if ($UninstallScript) {
      "powershell -NoProfile -ExecutionPolicy Bypass -File `"$UninstallScript`" -InstallDir `"$InstallDir`""
    } else {
      "powershell -NoProfile -ExecutionPolicy Bypass -Command `"& ([scriptblock]::Create((irm https://raw.githubusercontent.com/$Repo/main/uninstall.ps1))) -InstallDir '$InstallDir'`""
    }
    $UninstKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\PlayDLDownloadManager"
    New-Item -Path $UninstKey -Force | Out-Null
    $SizeKb = [int](((Get-ChildItem $InstallDir -Recurse -File -ErrorAction SilentlyContinue |
      Measure-Object -Property Length -Sum).Sum) / 1KB)
    $Props = [ordered]@{
      DisplayName     = "PlayDL Download Manager"
      DisplayVersion  = $Ver
      Publisher       = "leeymxz"
      URLInfoAbout    = "https://github.com/$Repo"
      DisplayIcon     = "$GuiExe,0"
      InstallLocation = $InstallDir
      UninstallString = $UninstallCmd
      NoModify        = 1
      NoRepair        = 1
      EstimatedSize   = $SizeKb
    }
    foreach ($k in $Props.Keys) {
      $type = if ($Props[$k] -is [int]) { "DWord" } else { "String" }
      New-ItemProperty -Path $UninstKey -Name $k -Value $Props[$k] -PropertyType $type -Force | Out-Null
    }
    Write-Host "registered PlayDL in Apps & features"

    if (Test-Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\PlayDL") {
      Write-Warning "PlayDL is also installed by the .exe installer; uninstall that copy from Apps & features to avoid running two installs."
    }
  }

  $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
  if (($UserPath -split ";") -notcontains $InstallDir) {
    $NewPath = if ($UserPath) { "$UserPath;$InstallDir" } else { $InstallDir }
    [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
    Write-Host "added $InstallDir to your user PATH (open a new terminal to pick it up)"
  }

  Write-Host "done."
}
finally {
  Remove-Item $Tmp -Recurse -Force -ErrorAction SilentlyContinue
}