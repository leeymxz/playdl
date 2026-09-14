$ErrorActionPreference = "Stop"
$token = $env:GITHUB_TOKEN
if (-not $token) { Write-Output "ERROR: set GITHUB_TOKEN first"; exit 1 }
$headers = @{ Authorization = "token $token" }

$body = "PlayDL v0.3.4 更新内容`n`n- Fix connection ceiling: 64/128 now truly applies (was clamped to 32)`n- Douyin filename fix (index.html -> video title)`n- New 3D Logo`n- All platforms synced"

$jsonBody = @{
    tag_name = "v0.3.4"
    name = "PlayDL v0.3.4"
    body = $body
    draft = $false
    prerelease = $false
} | ConvertTo-Json

$rel = Invoke-RestMethod -Uri "https://api.github.com/repos/leeymxz/playdl/releases" -Method POST -Headers $headers -Body $jsonBody -ContentType "application/json; charset=utf-8"
Write-Output "OK v0.3.4 release created"
Write-Output "ID: $($rel.id)"