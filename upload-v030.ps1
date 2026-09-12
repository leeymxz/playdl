$ErrorActionPreference = "Stop"
$token = $env:GITHUB_TOKEN
if (-not $token) { Write-Output "ERROR: no token"; exit 1 }
$headers = @{ Authorization = "token $token" }
$dir = "C:\Users\Administrator\Documents\Loomy Workspace\my-downloader"
$relId = 387467246  # 复用 v0.2.0 的 release？不，要新建 v0.3.0

# 先检查 v0.3.0 release 是否存在（Actions 触发后自动创建）
$rel = $null
try {
    $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/leeymxz/playdl/releases/tags/v0.3.0" -Headers $headers -ErrorAction Stop
} catch {}

if (-not $rel) {
    Write-Output "v0.3.0 release 还没创建（等 Actions 触发）"
    Write-Output "当前 Releases:"
    $rels = Invoke-RestMethod -Uri "https://api.github.com/repos/leeymxz/playdl/releases" -Headers $headers
    $rels | Select-Object -First 3 | ForEach-Object { Write-Output "  $($_.tag_name) ($($_.draft))" }
    exit 0
}

$relId = $rel.id
Write-Output "v0.3.0 Release ID: $relId"

$uploads = @(
    @{ File = "$dir\packaging\dist\PlayDL-Setup-0.3.0.exe"; Name = "PlayDL-Setup-0.3.0.exe" },
    @{ File = "$dir\dist\playdl-extension-chrome.zip"; Name = "playdl-extension-chrome.zip" },
    @{ File = "$dir\dist\playdl-extension-firefox.zip"; Name = "playdl-extension-firefox.zip" }
)
foreach ($u in $uploads) {
    if (-not (Test-Path $u.File)) { Write-Output "SKIP: $($u.Name)"; continue }
    $cur = Invoke-RestMethod -Uri "https://api.github.com/repos/leeymxz/playdl/releases/$relId" -Headers $headers
    $old = $cur.assets | Where-Object { $_.name -eq $u.Name }
    if ($old) { Invoke-RestMethod -Uri $old.url -Method DELETE -Headers $headers }
    $url = "https://uploads.github.com/repos/leeymxz/playdl/releases/$relId/assets?name=$($u.Name)"
    $h = @{ Authorization = "token $token"; "Content-Type" = "application/octet-stream" }
    $up = Invoke-RestMethod -Uri $url -Method POST -Headers $h -InFile $u.File
    Write-Output "Uploaded: $($u.Name) ($($up.size) bytes)"
}
Write-Output "DONE"