Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

# 查找 playdl.exe
$playdl = $null
$dirs = @((Split-Path $PSCommandPath -Parent), (Get-Location).Path)
foreach ($d in $dirs) {
    foreach ($name in @("playdl.exe", "pdl.exe")) {
        $p = Join-Path $d $name
        if (Test-Path $p) { $playdl = $p; break }
    }
    if ($playdl) { break }
}

if (-not $playdl) {
    [System.Windows.Forms.MessageBox]::Show("未找到 playdl.exe`n请把本脚本放在 playdl 同目录下", "PlayDL", "OK", "Error")
    exit
}

# 启动 PowerShell GUI
& powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path (Split-Path $PSCommandPath -Parent) "PlayDL.ps1")