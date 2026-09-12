$ErrorActionPreference = "Stop"
$token = $env:GITHUB_TOKEN
if (-not $token) { Write-Output "ERROR: no token"; exit 1 }
$headers = @{ Authorization = "token $token" }
$dir = "C:\Users\Administrator\Documents\Loomy Workspace\my-downloader"

# 创建/查找 v0.2.0 release
$rel = Invoke-RestMethod -Uri "https://api.github.com/repos/leeymxz/playdl/releases/tags/v0.2.0" -Headers $headers -ErrorAction SilentlyContinue
if (-not $rel) {
    Write-Output "v0.2.0 release 不存在，等待 Actions 创建..."
    exit 0
}
$relId = $rel.id
Write-Output "Release ID: $relId"

$uploads = @(
    @{ File = "$dir\packaging\dist\PlayDL-Setup-0.2.0.exe"; Name = "PlayDL-Setup-0.2.0.exe" },
    @{ File = "$dir\dist\playdl-extension-chrome.zip"; Name = "playdl-extension-chrome.zip" },
    @{ File = "$dir\dist\playdl-extension-firefox.zip"; Name = "playdl-extension-firefox.zip" }
)

foreach ($u in $uploads) {
    if (-not (Test-Path $u.File)) { Write-Output "SKIP: $($u.Name) missing"; continue }
    # 删同名旧资产
    $cur = Invoke-RestMethod -Uri "https://api.github.com/repos/leeymxz/playdl/releases/$relId" -Headers $headers
    $old = $cur.assets | Where-Object { $_.name -eq $u.Name }
    if ($old) {
        Invoke-RestMethod -Uri $old.url -Method DELETE -Headers $headers
        Write-Output "Deleted old: $($u.Name)"
    }
    $url = "https://uploads.github.com/repos/leeymxz/playdl/releases/$relId/assets?name=$($u.Name)"
    $h = @{ Authorization = "token $token"; "Content-Type" = "application/octet-stream" }
    $up = Invoke-RestMethod -Uri $url -Method POST -Headers $h -InFile $u.File
    Write-Output "Uploaded: $($u.Name) ($($up.size) bytes)"
}
Write-Output "DONE"