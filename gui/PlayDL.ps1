# PlayDL 下载器 - Windows GUI
# 需要安装 PlayDL CLI (playdl.exe) 在系统 PATH 或同目录下
# 双击 PlayDL.bat 启动，或右键 "用 PowerShell 运行"

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName PresentationFramework

# ============================================================
# 全局状态
# ============================================================
$script:PlayDLPath = $null
$script:DownloadJobs = @()          # 列表: @{Url, FileName, Status, Progress, Id}
$script:IsDownloading = $false
$script:CancellationRequested = $false

# ============================================================
# 查找 playdl.exe
# ============================================================
function Find-PlayDL {
    # 先找同目录
    $sameDir = Join-Path (Split-Path $PSCommandPath -Parent) "playdl.exe"
    if (Test-Path $sameDir) { return $sameDir }

    # 再找 pdl.exe (缩写)
    $pdlDir = Join-Path (Split-Path $PSCommandPath -Parent) "pdl.exe"
    if (Test-Path $pdlDir) { return $pdlDir }

    # 最后找 PATH
    $which = Get-Command "playdl.exe" -ErrorAction SilentlyContinue
    if ($which) { return $which.Source }

    $which2 = Get-Command "pdl.exe" -ErrorAction SilentlyContinue
    if ($which2) { return $which2.Source }

    return $null
}

# ============================================================
# 格式化文件大小
# ============================================================
function Format-Size {
    param([long]$Bytes)
    if ($Bytes -lt 1KB) { return "$Bytes B" }
    if ($Bytes -lt 1MB) { return "{0:N1} KB" -f ($Bytes / 1KB) }
    if ($Bytes -lt 1GB) { return "{0:N1} MB" -f ($Bytes / 1MB) }
    return "{0:N2} GB" -f ($Bytes / 1GB)
}

# ============================================================
# 下载核心逻辑
# ============================================================
function Start-Download {
    param([string]$Url, [string]$OutputDir)

    if ([string]::IsNullOrWhiteSpace($Url)) {
        [System.Windows.Forms.MessageBox]::Show("请输入下载链接！", "PlayDL", "OK", "Warning")
        return
    }

    if (-not ($Url -match "^https?://")) {
        [System.Windows.Forms.MessageBox]::Show("链接格式不正确，需要 http:// 或 https:// 开头`n$Url", "PlayDL", "OK", "Warning")
        return
    }

    # 提取文件名
    $fileName = [System.IO.Path]::GetFileName(($Url -split '/')[-1])
    if ([string]::IsNullOrWhiteSpace($fileName)) { $fileName = "download" }
    if (-not $fileName.Contains('.')) { $fileName += ".bin" }

    $dlPath = if ($OutputDir) { Join-Path $OutputDir $fileName } else { $fileName }

    # 添加到列表
    $job = @{
        Url = $Url
        FileName = $fileName
        Status = "排队中..."
        Progress = 0
        OutputPath = $dlPath
    }
    $script:DownloadJobs += $job
    Update-JobList

    $statusLabel.Text = "⏳ 正在下载: $fileName"
    $progressBar.Value = 0
    $script:IsDownloading = $true
    $script:CancellationRequested = $false

    # 异步执行下载
    $jobIndex = $script:DownloadJobs.Count - 1
    $ps = [PowerShell]::Create()

    $runspace = [RunspaceFactory]::CreateRunspace()
    $runspace.Open()
    $ps.Runspace = $runspace

    [void]$ps.AddScript({
        param($exe, $url, $outPath)

        # 构建参数
        $args = @("--json", "-o", $outPath, $url)
        Write-Output ">>> $exe $($args -join ' ')"

        $psi = New-Object System.Diagnostics.ProcessStartInfo
        $psi.FileName = $exe
        $psi.Arguments = $args
        $psi.UseShellExecute = $false
        $psi.RedirectStandardOutput = $true
        $psi.RedirectStandardError = $true
        $psi.CreateNoWindow = $true

        $proc = [System.Diagnostics.Process]::Start($psi)
        $output = $proc.StandardOutput.ReadToEnd()
        $err = $proc.StandardError.ReadToEnd()
        $proc.WaitForExit()

        return @{ ExitCode = $proc.ExitCode; Output = $output; Error = $err }
    }).AddArguments($script:PlayDLPath, $Url, $dlPath)

    # 异步调用完成后的回调
    $handle = $ps.BeginInvoke()

    # 启动定时器监控进度
    $timer = New-Object System.Windows.Forms.Timer
    $timer.Interval = 500
    $timer.Add_Tick({
        if ($handle.IsCompleted) {
            $timer.Stop()
            $timer.Dispose()

            $result = $ps.EndInvoke($handle)
            $data = $result[0]
            $exitCode = $data.ExitCode
            $output = $data.Output

            $ps.Dispose()
            $runspace.Dispose()

            $script:IsDownloading = $false

            if ($exitCode -eq 0) {
                $size = 0
                if (Test-Path $dlPath) { $size = (Get-Item $dlPath).Length }
                $script:DownloadJobs[$jobIndex].Status = "✅ 完成 ($(Format-Size $size))"
                $script:DownloadJobs[$jobIndex].Progress = 100
                $statusLabel.Text = "✅ 下载完成: $fileName ($(Format-Size $size))"
                $progressBar.Value = 100
                $btnDownload.Enabled = $true
                $btnDownload.Text = "⬇ 开始下载"
            } else {
                $script:DownloadJobs[$jobIndex].Status = "❌ 失败"
                $statusLabel.Text = "❌ 下载失败: $fileName"
                $progressBar.Value = 0
                $btnDownload.Enabled = $true
                $btnDownload.Text = "⬇ 开始下载"
            }
            Update-JobList
        } else {
            # 模拟进度（没有实时反馈，假装动一下）
            $current = $progressBar.Value
            if ($current -lt 90) {
                $progressBar.Value = [Math]::Min($current + (Get-Random -Min 1 -Max 5), 90)
            }
        }
    })
    $timer.Start()

    $btnDownload.Enabled = $false
    $btnDownload.Text = "⏳ 下载中..."
}

# ============================================================
# 更新任务列表
# ============================================================
function Update-JobList {
    $listBox.Items.Clear()
    for ($i = 0; $i -lt $script:DownloadJobs.Count; $i++) {
        $j = $script:DownloadJobs[$i]
        $display = "[$($j.Status)] $($j.FileName)"
        [void]$listBox.Items.Add($display)
    }
    if ($script:DownloadJobs.Count -gt 0) {
        $listBox.TopIndex = $listBox.Items.Count - 1
    }
}

# ============================================================
# 检查 playdl 路径
# ============================================================
$script:PlayDLPath = Find-PlayDL

# ============================================================
# 创建窗体
# ============================================================
$form = New-Object System.Windows.Forms.Form
$form.Text = "PlayDL 下载器"
$form.Size = New-Object System.Drawing.Size(720, 520)
$form.StartPosition = "CenterScreen"
$form.MinimumSize = New-Object System.Drawing.Size(600, 400)
$form.Icon = [System.Drawing.Icon]::ExtractAssociatedIcon($PSCommandPath)
$form.BackColor = "#1a1a2e"
$form.Font = New-Object System.Drawing.Font("Segoe UI", 10)

# ---- 顶部标题栏 ----
$headerPanel = New-Object System.Windows.Forms.Panel
$headerPanel.Size = New-Object System.Drawing.Size(720, 60)
$headerPanel.BackColor = "#16213e"
$headerPanel.Dock = "Top"

$titleLabel = New-Object System.Windows.Forms.Label
$titleLabel.Text = "⚡ PlayDL 高速下载器"
$titleLabel.ForeColor = "#00d4ff"
$titleLabel.Font = New-Object System.Drawing.Font("Segoe UI", 16, [System.Drawing.FontStyle]::Bold)
$titleLabel.Size = New-Object System.Drawing.Size(300, 40)
$titleLabel.Location = New-Object System.Drawing.Point(20, 10)
$headerPanel.Controls.Add($titleLabel)

$statusLabel = New-Object System.Windows.Forms.Label
$statusLabel.Text = if ($script:PlayDLPath) { "✅ 引擎就绪" } else { "❌ 未找到 playdl.exe" }
$statusLabel.ForeColor = if ($script:PlayDLPath) { "#00ff88" } else { "#ff4444" }
$statusLabel.Font = New-Object System.Drawing.Font("Segoe UI", 9)
$statusLabel.Size = New-Object System.Drawing.Size(350, 25)
$statusLabel.Location = New-Object System.Drawing.Point(350, 18)
$statusLabel.TextAlign = "Right"
$headerPanel.Controls.Add($statusLabel)

$form.Controls.Add($headerPanel)

# ---- 输入区域 ----
$inputGroup = New-Object System.Windows.Forms.GroupBox
$inputGroup.Text = "  下载链接"
$inputGroup.ForeColor = "#00d4ff"
$inputGroup.Font = New-Object System.Drawing.Font("Segoe UI", 10, [System.Drawing.FontStyle]::Bold)
$inputGroup.Size = New-Object System.Drawing.Size(680, 60)
$inputGroup.Location = New-Object System.Drawing.Point(20, 75)
$inputGroup.BackColor = "#1a1a2e"
$form.Controls.Add($inputGroup)

$txtUrl = New-Object System.Windows.Forms.TextBox
$txtUrl.Size = New-Object System.Drawing.Size(480, 25)
$txtUrl.Location = New-Object System.Drawing.Point(10, 25)
$txtUrl.Font = New-Object System.Drawing.Font("Consolas", 10)
$txtUrl.BackColor = "#0f3460"
$txtUrl.ForeColor = "#ffffff"
$txtUrl.BorderStyle = "FixedSingle"
$inputGroup.Controls.Add($txtUrl)

$btnDownload = New-Object System.Windows.Forms.Button
$btnDownload.Text = "⬇ 开始下载"
$btnDownload.Size = New-Object System.Drawing.Size(170, 30)
$btnDownload.Location = New-Object System.Drawing.Point(500, 22)
$btnDownload.BackColor = "#00d4ff"
$btnDownload.ForeColor = "#1a1a2e"
$btnDownload.Font = New-Object System.Drawing.Font("Segoe UI", 10, [System.Drawing.FontStyle]::Bold)
$btnDownload.FlatStyle = "Flat"
$btnDownload.Cursor = "Hand"
$inputGroup.Controls.Add($btnDownload)

# ---- 进度条 ----
$progressBar = New-Object System.Windows.Forms.ProgressBar
$progressBar.Size = New-Object System.Drawing.Size(680, 20)
$progressBar.Location = New-Object System.Drawing.Point(20, 145)
$progressBar.Style = "Continuous"
$progressBar.ForeColor = "#00d4ff"
$progressBar.BackColor = "#0f3460"
$progressBar.Value = 0
$form.Controls.Add($progressBar)

# ---- 任务列表分组 ----
$listGroup = New-Object System.Windows.Forms.GroupBox
$listGroup.Text = "  下载记录"
$listGroup.ForeColor = "#00d4ff"
$listGroup.Font = New-Object System.Drawing.Font("Segoe UI", 10, [System.Drawing.FontStyle]::Bold)
$listGroup.Size = New-Object System.Drawing.Size(680, 250)
$listGroup.Location = New-Object System.Drawing.Point(20, 175)
$listGroup.BackColor = "#1a1a2e"
$form.Controls.Add($listGroup)

$listBox = New-Object System.Windows.Forms.ListBox
$listBox.Size = New-Object System.Drawing.Size(660, 200)
$listBox.Location = New-Object System.Drawing.Point(10, 20)
$listBox.BackColor = "#0f3460"
$listBox.ForeColor = "#ffffff"
$listBox.Font = New-Object System.Drawing.Font("Consolas", 9)
$listBox.BorderStyle = "FixedSingle"
$listGroup.Controls.Add($listBox)

# ---- 底部按钮 ----
$bottomPanel = New-Object System.Windows.Forms.Panel
$bottomPanel.Size = New-Object System.Drawing.Size(720, 50)
$bottomPanel.Location = New-Object System.Drawing.Point(0, 435)
$bottomPanel.BackColor = "#16213e"

$btnClear = New-Object System.Windows.Forms.Button
$btnClear.Text = "🗑 清空记录"
$btnClear.Size = New-Object System.Drawing.Size(120, 30)
$btnClear.Location = New-Object System.Drawing.Point(20, 10)
$btnClear.BackColor = "#e94560"
$btnClear.ForeColor = "#ffffff"
$btnClear.FlatStyle = "Flat"
$btnClear.Cursor = "Hand"
$bottomPanel.Controls.Add($btnClear)

$btnOpenDir = New-Object System.Windows.Forms.Button
$btnOpenDir.Text = "📂 打开下载目录"
$btnOpenDir.Size = New-Object System.Drawing.Size(140, 30)
$btnOpenDir.Location = New-Object System.Drawing.Point(155, 10)
$btnOpenDir.BackColor = "#0f3460"
$btnOpenDir.ForeColor = "#00d4ff"
$btnOpenDir.FlatStyle = "Flat"
$btnOpenDir.Cursor = "Hand"
$bottomPanel.Controls.Add($btnOpenDir)

$versionLabel = New-Object System.Windows.Forms.Label
$versionLabel.Text = "PlayDL v0.1.0 | 基于 Hydra 引擎"
$versionLabel.ForeColor = "#555577"
$versionLabel.Font = New-Object System.Drawing.Font("Segoe UI", 8)
$versionLabel.Size = New-Object System.Drawing.Size(300, 20)
$versionLabel.Location = New-Object System.Drawing.Point(410, 15)
$versionLabel.TextAlign = "Right"
$bottomPanel.Controls.Add($versionLabel)

$form.Controls.Add($bottomPanel)

# ---- 事件绑定 ----
$btnDownload.Add_Click({
    $url = $txtUrl.Text.Trim()
    Start-Download -Url $url
})

$txtUrl.Add_KeyDown({
    if ($_.KeyCode -eq "Enter" -and -not $script:IsDownloading) {
        $btnDownload.PerformClick()
    }
})

$btnClear.Add_Click({
    $script:DownloadJobs = @()
    Update-JobList
    $progressBar.Value = 0
    $statusLabel.Text = if ($script:PlayDLPath) { "✅ 引擎就绪" } else { "❌ 未找到 playdl.exe" }
})

$btnOpenDir.Add_Click({
    $dlDir = (Get-Location).Path
    Start-Process explorer.exe $dlDir
})

# ---- 如果没有 playdl，显示提示 ----
if (-not $script:PlayDLPath) {
    $statusLabel.Text = "❌ 未找到 playdl.exe，请先编译或下载"
    $btnDownload.Enabled = $false
}

# ---- 键盘快捷键 ----
$form.KeyPreview = $true

# ---- 深色主题微调颜色 ----
$form.Controls | ForEach-Object {
    if ($_ -is [System.Windows.Forms.GroupBox]) {
        $_.BackColor = "#1a1a2e"
    }
}

# ---- 启动 ----
[void]$form.ShowDialog()