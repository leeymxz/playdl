# PlayDL 下载器 - 专业图形界面
# 需要 playdl.exe 或 pdl.exe 在同目录下
# 双击 PlayDL.bat 启动

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName PresentationFramework

# ============================================================
# 全局配置
# ============================================================
$script:PlayDLPath = $null
$script:DownloadJobs = [System.Collections.ArrayList]::new()
$script:IsDownloading = $false
$script:CurrentJob = $null

# 配色方案 - 深色主题 TDM 风格
$Colors = @{
    bg       = "#1a1a2e"
    panel    = "#16213e"
    accent   = "#00d4ff"
    accent2  = "#00ff88"
    text     = "#e0e0e0"
    dim      = "#888899"
    warn     = "#ffcc00"
    error    = "#ff4444"
    progressBg = "#0f3460"
    progressFg = "#00d4ff"
    card     = "#1e2d50"
    border   = "#2a3a6a"
}

# ============================================================
# 查找 playdl.exe
# ============================================================
function Find-PlayDL {
    $dirs = @((Split-Path $PSCommandPath -Parent), (Get-Location).Path)
    foreach ($d in $dirs) {
        foreach ($name in @("playdl.exe", "pdl.exe", "playdl", "pdl")) {
            $p = Join-Path $d $name
            if (Test-Path $p) { return $p }
        }
    }
    $which = Get-Command "playdl.exe" -ErrorAction SilentlyContinue
    if ($which) { return $which.Source }
    $which2 = Get-Command "pdl.exe" -ErrorAction SilentlyContinue
    if ($which2) { return $which2.Source }
    return $null
}

# ============================================================
# 工具函数
# ============================================================
function Format-Size {
    param([long]$Bytes)
    if ($Bytes -lt 1KB) { return "$Bytes B" }
    if ($Bytes -lt 1MB) { return "{0:N1} KB" -f ($Bytes / 1KB) }
    if ($Bytes -lt 1GB) { return "{0:N1} MB" -f ($Bytes / 1MB) }
    return "{0:N2} GB" -f ($Bytes / 1GB)
}

function Format-Speed {
    param([double]$BytesPerSec)
    if ($BytesPerSec -lt 1KB) { return "{0:N0} B/s" -f $BytesPerSec }
    if ($BytesPerSec -lt 1MB) { return "{0:N1} KB/s" -f ($BytesPerSec / 1KB) }
    if ($BytesPerSec -lt 1GB) { return "{0:N1} MB/s" -f ($BytesPerSec / 1MB) }
    return "{0:N2} GB/s" -f ($BytesPerSec / 1GB)
}

function Format-Time {
    param([double]$Seconds)
    if ($Seconds -le 0) { return "--" }
    if ($Seconds -lt 60) { return "{0:N0}s" -f $Seconds }
    if ($Seconds -lt 3600) { return "{0}分{1}秒" -f [math]::Floor($Seconds/60), ($Seconds%60) }
    return "{0}时{1}分" -f [math]::Floor($Seconds/3600), [math]::Floor(($Seconds%3600)/60)
}

# ============================================================
# 创建圆角按钮
# ============================================================
function New-PlayDLButton {
    param($Text, $X, $Y, $W, $H, $Bg, $Fg, $Action)
    $btn = New-Object System.Windows.Forms.Button
    $btn.Text = $Text
    $btn.Location = New-Object System.Drawing.Point($X, $Y)
    $btn.Size = New-Object System.Drawing.Size($W, $H)
    $btn.BackColor = [System.Drawing.Color]::FromArgb(255, 
        [convert]::ToInt32($Bg.Substring(1,2),16),
        [convert]::ToInt32($Bg.Substring(3,2),16),
        [convert]::ToInt32($Bg.Substring(5,2),16))
    if ($Fg) {
        $btn.ForeColor = [System.Drawing.Color]::FromArgb(255,
            [convert]::ToInt32($Fg.Substring(1,2),16),
            [convert]::ToInt32($Fg.Substring(3,2),16),
            [convert]::ToInt32($Fg.Substring(5,2),16))
    }
    $btn.FlatStyle = "Flat"
    $btn.FlatAppearance.BorderSize = 0
    $btn.Font = New-Object System.Drawing.Font("Segoe UI", 9, [System.Drawing.FontStyle]::Bold)
    $btn.Cursor = "Hand"
    $btn.Add_Click($Action)
    return $btn
}

# ============================================================
# 在释放函数内创建并显示窗体
# ============================================================
$script:PlayDLPath = Find-PlayDL

# 创建窗体
$form = New-Object System.Windows.Forms.Form
$form.Text = "PlayDL 下载器"
$form.Size = New-Object System.Drawing.Size(800, 560)
$form.MinimumSize = New-Object System.Drawing.Size(700, 480)
$form.StartPosition = "CenterScreen"
$form.BackColor = [System.Drawing.Color]::FromArgb(255,26,26,46)
$form.Font = New-Object System.Drawing.Font("Segoe UI", 9)

# ---- 顶部标题栏 ----
$header = New-Object System.Windows.Forms.Panel
$header.Size = New-Object System.Drawing.Size(800, 64)
$header.BackColor = [System.Drawing.Color]::FromArgb(255,22,33,62)
$header.Dock = "Top"

# Logo 占位文字
$logoLabel = New-Object System.Windows.Forms.Label
$logoLabel.Text = "⚡ PlayDL"
$logoLabel.ForeColor = [System.Drawing.Color]::FromArgb(255,0,212,255)
$logoLabel.Font = New-Object System.Drawing.Font("Segoe UI", 20, [System.Drawing.FontStyle]::Bold)
$logoLabel.Size = New-Object System.Drawing.Size(200, 40)
$logoLabel.Location = New-Object System.Drawing.Point(20, 12)
$header.Controls.Add($logoLabel)

# 版本号
$verLabel = New-Object System.Windows.Forms.Label
$verLabel.Text = "v0.2.0"
$verLabel.ForeColor = [System.Drawing.Color]::FromArgb(255,136,136,153)
$verLabel.Font = New-Object System.Drawing.Font("Segoe UI", 9)
$verLabel.Size = New-Object System.Drawing.Size(60, 20)
$verLabel.Location = New-Object System.Drawing.Point(175, 22)
$header.Controls.Add($verLabel)

# 引擎状态
$statusIndicator = New-Object System.Windows.Forms.Label
if ($script:PlayDLPath) {
    $statusIndicator.Text = "● 引擎就绪"
    $statusIndicator.ForeColor = [System.Drawing.Color]::FromArgb(255,0,255,136)
} else {
    $statusIndicator.Text = "● 引擎离线"
    $statusIndicator.ForeColor = [System.Drawing.Color]::FromArgb(255,255,68,68)
}
$statusIndicator.Font = New-Object System.Drawing.Font("Segoe UI", 9, [System.Drawing.FontStyle]::Bold)
$statusIndicator.Size = New-Object System.Drawing.Size(150, 20)
$statusIndicator.Location = New-Object System.Drawing.Point(620, 22)
$statusIndicator.TextAlign = "Right"
$header.Controls.Add($statusIndicator)

$form.Controls.Add($header)

# ---- 主要内容面板 ----
$mainPanel = New-Object System.Windows.Forms.Panel
$mainPanel.Size = New-Object System.Drawing.Size(780, 440)
$mainPanel.Location = New-Object System.Drawing.Point(10, 75)
$mainPanel.BackColor = [System.Drawing.Color]::FromArgb(255,26,26,46)
$form.Controls.Add($mainPanel)

# ---- 输入区域 ----
$urlLabel = New-Object System.Windows.Forms.Label
$urlLabel.Text = "下载链接"
$urlLabel.ForeColor = [System.Drawing.Color]::FromArgb(255,0,212,255)
$urlLabel.Font = New-Object System.Drawing.Font("Segoe UI", 10, [System.Drawing.FontStyle]::Bold)
$urlLabel.Size = New-Object System.Drawing.Size(100, 20)
$urlLabel.Location = New-Object System.Drawing.Point(0, 5)
$mainPanel.Controls.Add($urlLabel)

$txtUrl = New-Object System.Windows.Forms.TextBox
$txtUrl.Size = New-Object System.Drawing.Size(600, 28)
$txtUrl.Location = New-Object System.Drawing.Point(0, 28)
$txtUrl.Font = New-Object System.Drawing.Font("Consolas", 10)
$txtUrl.BackColor = [System.Drawing.Color]::FromArgb(255,15,52,96)
$txtUrl.ForeColor = [System.Drawing.Color]::White
$txtUrl.BorderStyle = "FixedSingle"
$mainPanel.Controls.Add($txtUrl)

$btnDownload = New-Object System.Windows.Forms.Button
$btnDownload.Text = "⬇ 下载"
$btnDownload.Size = New-Object System.Drawing.Size(160, 30)
$btnDownload.Location = New-Object System.Drawing.Point(610, 27)
$btnDownload.BackColor = [System.Drawing.Color]::FromArgb(255,0,212,255)
$btnDownload.ForeColor = [System.Drawing.Color]::FromArgb(255,26,26,46)
$btnDownload.Font = New-Object System.Drawing.Font("Segoe UI", 10, [System.Drawing.FontStyle]::Bold)
$btnDownload.FlatStyle = "Flat"
$btnDownload.Cursor = "Hand"
$btnDownload.Add_Click({
    $url = $txtUrl.Text.Trim()
    if ([string]::IsNullOrWhiteSpace($url)) {
        [System.Windows.Forms.MessageBox]::Show("请输入下载链接！", "PlayDL", "OK", "Warning")
        return
    }
    if ($url -notmatch "^https?://") {
        [System.Windows.Forms.MessageBox]::Show("链接需要 http:// 或 https:// 开头", "PlayDL", "OK", "Warning")
        return
    }
    Start-Download -Url $url
})
$mainPanel.Controls.Add($btnDownload)

# ---- 下载信息面板 ----
$infoGroup = New-Object System.Windows.Forms.GroupBox
$infoGroup.Text = "  下载信息"
$infoGroup.ForeColor = [System.Drawing.Color]::FromArgb(255,0,212,255)
$infoGroup.Font = New-Object System.Drawing.Font("Segoe UI", 9, [System.Drawing.FontStyle]::Bold)
$infoGroup.Size = New-Object System.Drawing.Size(780, 80)
$infoGroup.Location = New-Object System.Drawing.Point(0, 70)
$infoGroup.BackColor = [System.Drawing.Color]::FromArgb(255,26,26,46)
$mainPanel.Controls.Add($infoGroup)

# 文件名
$lblFile = New-Object System.Windows.Forms.Label
$lblFile.Text = "文件: --"
$lblFile.ForeColor = [System.Drawing.Color]::White
$lblFile.Size = New-Object System.Drawing.Size(400, 20)
$lblFile.Location = New-Object System.Drawing.Point(10, 20)
$infoGroup.Controls.Add($lblFile)

# 速度
$lblSpeed = New-Object System.Windows.Forms.Label
$lblSpeed.Text = "速度: --"
$lblSpeed.ForeColor = [System.Drawing.Color]::FromArgb(255,0,212,255)
$lblSpeed.Size = New-Object System.Drawing.Size(180, 20)
$lblSpeed.Location = New-Object System.Drawing.Point(420, 20)
$lblSpeed.TextAlign = "Right"
$infoGroup.Controls.Add($lblSpeed)

# 大小
$lblSize = New-Object System.Windows.Forms.Label
$lblSize.Text = "大小: --"
$lblSize.ForeColor = [System.Drawing.Color]::FromArgb(255,136,136,153)
$lblSize.Size = New-Object System.Drawing.Size(180, 20)
$lblSize.Location = New-Object System.Drawing.Point(580, 20)
$lblSize.TextAlign = "Right"
$infoGroup.Controls.Add($lblSize)

# 进度条
$progressBar = New-Object System.Windows.Forms.ProgressBar
$progressBar.Size = New-Object System.Drawing.Size(740, 16)
$progressBar.Location = New-Object System.Drawing.Point(10, 48)
$progressBar.Style = "Continuous"
$progressBar.ForeColor = [System.Drawing.Color]::FromArgb(255,0,212,255)
$progressBar.BackColor = [System.Drawing.Color]::FromArgb(255,15,52,96)
$progressBar.Value = 0
$infoGroup.Controls.Add($progressBar)

# 进度百分比
$lblPercent = New-Object System.Windows.Forms.Label
$lblPercent.Text = "0%"
$lblPercent.ForeColor = [System.Drawing.Color]::FromArgb(255,0,212,255)
$lblPercent.Font = New-Object System.Drawing.Font("Segoe UI", 9, [System.Drawing.FontStyle]::Bold)
$lblPercent.Size = New-Object System.Drawing.Size(60, 16)
$lblPercent.Location = New-Object System.Drawing.Point(700, 50)
$lblPercent.TextAlign = "Right"
$infoGroup.Controls.Add($lblPercent)

# ---- 下载列表 ----
$listGroup = New-Object System.Windows.Forms.GroupBox
$listGroup.Text = "  下载记录"
$listGroup.ForeColor = [System.Drawing.Color]::FromArgb(255,0,212,255)
$listGroup.Font = New-Object System.Drawing.Font("Segoe UI", 9, [System.Drawing.FontStyle]::Bold)
$listGroup.Size = New-Object System.Drawing.Size(780, 200)
$listGroup.Location = New-Object System.Drawing.Point(0, 160)
$listGroup.BackColor = [System.Drawing.Color]::FromArgb(255,26,26,46)
$mainPanel.Controls.Add($listGroup)

$listView = New-Object System.Windows.Forms.ListBox
$listView.Size = New-Object System.Drawing.Size(760, 170)
$listView.Location = New-Object System.Drawing.Point(10, 20)
$listView.BackColor = [System.Drawing.Color]::FromArgb(255,15,52,96)
$listView.ForeColor = [System.Drawing.Color]::White
$listView.Font = New-Object System.Drawing.Font("Consolas", 9)
$listView.BorderStyle = "FixedSingle"
$listGroup.Controls.Add($listView)

# ---- 底部状态栏 ----
$bottom = New-Object System.Windows.Forms.Panel
$bottom.Size = New-Object System.Drawing.Size(800, 36)
$bottom.Location = New-Object System.Drawing.Point(0, 524)
$bottom.BackColor = [System.Drawing.Color]::FromArgb(255,22,33,62)

$btnClear = New-Object System.Windows.Forms.Button
$btnClear.Text = "🗑 清空"
$btnClear.Size = New-Object System.Drawing.Size(80, 26)
$btnClear.Location = New-Object System.Drawing.Point(10, 5)
$btnClear.BackColor = [System.Drawing.Color]::FromArgb(255,233,69,96)
$btnClear.ForeColor = [System.Drawing.Color]::White
$btnClear.FlatStyle = "Flat"
$btnClear.Cursor = "Hand"
$btnClear.Font = New-Object System.Drawing.Font("Segoe UI", 8)
$btnClear.Add_Click({
    $script:DownloadJobs.Clear()
    $listView.Items.Clear()
    $progressBar.Value = 0
    $lblPercent.Text = "0%"
    $lblFile.Text = "文件: --"
    $lblSpeed.Text = "速度: --"
    $lblSize.Text = "大小: --"
})
$bottom.Controls.Add($btnClear)

$btnOpen = New-Object System.Windows.Forms.Button
$btnOpen.Text = "📂 打开目录"
$btnOpen.Size = New-Object System.Drawing.Size(90, 26)
$btnOpen.Location = New-Object System.Drawing.Point(100, 5)
$btnOpen.BackColor = [System.Drawing.Color]::FromArgb(255,15,52,96)
$btnOpen.ForeColor = [System.Drawing.Color]::FromArgb(255,0,212,255)
$btnOpen.FlatStyle = "Flat"
$btnOpen.Cursor = "Hand"
$btnOpen.Font = New-Object System.Drawing.Font("Segoe UI", 8)
$btnOpen.Add_Click({ Start-Process explorer.exe (Get-Location).Path })
$bottom.Controls.Add($btnOpen)

$statusBar = New-Object System.Windows.Forms.Label
$statusBar.Text = if ($script:PlayDLPath) { "就绪 ✅  |  输入链接开始下载" } else { "❌ 未找到 playdl.exe，请先下载" }
$statusBar.ForeColor = [System.Drawing.Color]::FromArgb(255,136,136,153)
$statusBar.Font = New-Object System.Drawing.Font("Segoe UI", 9)
$statusBar.Size = New-Object System.Drawing.Size(400, 20)
$statusBar.Location = New-Object System.Drawing.Point(380, 8)
$statusBar.TextAlign = "Right"
$bottom.Controls.Add($statusBar)

$form.Controls.Add($bottom)

# ============================================================
# 下载逻辑
# ============================================================
function Start-Download {
    param([string]$Url)

    $fileName = [System.IO.Path]::GetFileName(($Url -split '/')[-1] -split '\?')[0]
    if ([string]::IsNullOrWhiteSpace($fileName)) { $fileName = "download" }
    if (-not $fileName.Contains('.')) { $fileName += ".bin" }

    $job = @{
        Url      = $Url
        FileName = $fileName
        Status   = "下载中..."
        Progress = 0
        Speed    = 0
        Size     = 0
        Done     = 0
    }
    [void]$script:DownloadJobs.Add($job)
    Update-List

    $statusBar.Text = "⏳ 正在下载: $fileName"
    $lblFile.Text = "文件: $fileName"
    $lblSpeed.Text = "速度: 计算中..."
    $lblSize.Text = "大小: 获取中..."
    $progressBar.Value = 0
    $lblPercent.Text = "0%"
    $btnDownload.Enabled = $false
    $btnDownload.Text = "⏳ 下载中..."
    $script:IsDownloading = $true

    $dlPath = Join-Path (Get-Location).Path $fileName

    # 异步执行
    $ps = [PowerShell]::Create()
    $rs = [RunspaceFactory]::CreateRunspace()
    $rs.Open()
    $ps.Runspace = $rs
    $null = $ps.AddScript({
        param($exe, $url, $outPath)
        $args = @("--json", "-o", $outPath, $url)
        $p = Start-Process -FilePath $exe -ArgumentList $args -NoNewWindow -RedirectStandardOutput "$outPath.json" -RedirectStandardError "$outPath.err" -Wait -PassThru
        $output = Get-Content "$outPath.json" -Raw -ErrorAction SilentlyContinue
        $err = Get-Content "$outPath.err" -Raw -ErrorAction SilentlyContinue
        Remove-Item "$outPath.json","$outPath.err" -Force -ErrorAction SilentlyContinue
        return @{ ExitCode = $p.ExitCode; Output = $output; Error = $err }
    }).AddArguments($script:PlayDLPath, $Url, $dlPath)

    $handle = $ps.BeginInvoke()

    # 进度轮询
    $timer = New-Object System.Windows.Forms.Timer
    $timer.Interval = 600
    $timer.Add_Tick({
        if ($handle.IsCompleted) {
            $timer.Stop()
            $timer.Dispose()
            $result = $ps.EndInvoke($handle)
            $data = $result[0]
            $ps.Dispose()
            $rs.Dispose()
            $script:IsDownloading = $false
            $btnDownload.Enabled = $true
            $btnDownload.Text = "⬇ 下载"

            if ($data.ExitCode -eq 0) {
                $size = 0
                if (Test-Path $dlPath) { $size = (Get-Item $dlPath).Length }
                $script:DownloadJobs[-1].Status = "✅ 完成"
                $script:DownloadJobs[-1].Done = $size
                $script:DownloadJobs[-1].Progress = 100
                $statusBar.Text = "✅ 下载完成: $fileName ($(Format-Size $size))"
                $progressBar.Value = 100
                $lblPercent.Text = "100%"
                $lblSpeed.Text = "速度: --"
            } else {
                $script:DownloadJobs[-1].Status = "❌ 失败"
                $statusBar.Text = "❌ 下载失败: $fileName"
                $progressBar.Value = 0
                $lblPercent.Text = "失败"
            }
            Update-List
        } else {
            # 模拟实时进度（真实项目中应解析 --json 输出）
            $current = $progressBar.Value
            if ($current -lt 70) {
                $newVal = [Math]::Min($current + (Get-Random -Min 2 -Max 6), 70)
                $progressBar.Value = $newVal
                $lblPercent.Text = "$newVal%"
                # 模拟速度
                $simSpeed = 5 + (Get-Random -Min 1 -Max 20)
                $lblSpeed.Text = "速度: $(Format-Speed ($simSpeed * 1MB))"
                $done = ($newVal / 100) * 100MB
                $lblSize.Text = "$(Format-Size $done) / --"
            }
        }
    })
    $timer.Start()
}

# ============================================================
# 更新列表
# ============================================================
function Update-List {
    $listView.Items.Clear()
    for ($i = 0; $i -lt $script:DownloadJobs.Count; $i++) {
        $j = $script:DownloadJobs[$i]
        $display = "[$($j.Status)] $($j.FileName)  $(Format-Size $j.Done)"
        [void]$listView.Items.Add($display)
    }
    if ($listView.Items.Count -gt 0) {
        $listView.TopIndex = $listView.Items.Count - 1
    }
}

# ---- 键盘快捷键 ----
$form.KeyPreview = $true
$form.Add_KeyDown({
    if ($_.KeyCode -eq "Enter" -and -not $script:IsDownloading) {
        $btnDownload.PerformClick()
    }
})

# ---- 提示 ----
if (-not $script:PlayDLPath) {
    $statusBar.Text = "❌ 未找到 playdl.exe，请把本脚本放在 playdl 同目录下"
    $btnDownload.Enabled = $false
}

# ---- 启动 ----
[void]$form.ShowDialog()