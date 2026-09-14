$ErrorActionPreference = "Stop"
$token = $env:GITHUB_TOKEN
if (-not $token) { Write-Output "ERROR"; exit 1 }
$headers = @{ Authorization = "token $token" }
$relId = 388167112
$tmp = "$env:TEMP\playdl-034"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

# 找到最近的 Release run
$r = Invoke-RestMethod -Uri "https://api.github.com/repos/leeymxz/playdl/actions/runs?per_page=1" -Headers $headers
$run = $r.workflow_runs | Where-Object { $_.name -eq "Release" } | Select-Object -First 1
$runId = $run.id
Write-Output "Run: $runId"

# 下载 4 个平台 artifact
$arts = Invoke-RestMethod -Uri "https://api.github.com/repos/leeymxz/playdl/actions/runs/$runId/artifacts" -Headers $headers
foreach ($a in $arts.artifacts) {
    $name = $a.name
    # 解析 os/arch
    if ($name -match "^playdl-(macos|linux|windows)-(arm64|amd64)$") {
        $os = $Matches[1]; $arch = $Matches[2]
        $dest = "$tmp\$name.zip"
        Write-Output "Downloading $name ..."
        Invoke-RestMethod -Uri $a.archive_download_url -Headers $headers -OutFile $dest
        # 解压
        Expand-Archive -Path $dest -DestinationPath "$tmp\$name" -Force
        # 找到里面的压缩包
        $inner = Get-ChildItem "$tmp\$name" -Filter "*.zip" -Recurse | Select-Object -First 1
        if (-not $inner) { $inner = Get-ChildItem "$tmp\$name" -Filter "*.tar.gz" -Recurse | Select-Object -First 1 }
        if ($inner) {
            # 上传到 release（用 playdl-{ver}-{os}-{arch}.ext 命名）
            $ext = $inner.Extension
            $upName = "playdl-0.3.4-$os-$arch$ext"
            $url = "https://uploads.github.com/repos/leeymxz/playdl/releases/$relId/assets?name=$upName"
            $h = @{ Authorization = "token $token"; "Content-Type" = "application/octet-stream" }
            $up = Invoke-RestMethod -Uri $url -Method POST -Headers $h -InFile $inner.FullName
            Write-Output "Uploaded: $upName ($([math]::Round($up.size/1KB,0)) KB)"
        }
    }
}
Write-Output "DONE"