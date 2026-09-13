$ErrorActionPreference = "Stop"
$token = $env:GITHUB_TOKEN
if (-not $token) { Write-Output "ERROR"; exit 1 }
$headers = @{ Authorization = "token $token" }
$dir = "C:\Users\Administrator\Documents\Loomy Workspace\my-downloader"

$rel = Invoke-RestMethod -Uri "https://api.github.com/repos/leeymxz/playdl/releases/tags/v0.3.2" -Headers $headers
$relId = $rel.id

# 删除旧的 setup 资产
$old = $rel.assets | Where-Object { $_.name -eq "PlayDL-0.3.2-windows-x64-setup.exe" }
if ($old) {
    Invoke-RestMethod -Uri $old.url -Method DELETE -Headers $headers
    Write-Output "Deleted old setup"
}

# 上传新的
$file = "$dir\packaging\dist\PlayDL-0.3.2-windows-x64-setup.exe"
$url = "https://uploads.github.com/repos/leeymxz/playdl/releases/$relId/assets?name=PlayDL-0.3.2-windows-x64-setup.exe"
$h = @{ Authorization = "token $token"; "Content-Type" = "application/octet-stream" }
$up = Invoke-RestMethod -Uri $url -Method POST -Headers $h -InFile $file
$kb = [math]::Round($up.size / 1KB, 0)
Write-Output "Uploaded: PlayDL-0.3.2-windows-x64-setup.exe ($kb KB)"
Write-Output "DONE"