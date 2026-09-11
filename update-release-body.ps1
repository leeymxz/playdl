$ErrorActionPreference = "Stop"
$token = $env:GITHUB_TOKEN
if (-not $token) {
    Write-Output "ERROR: set GITHUB_TOKEN env var first"
    exit 1
}
$headers = @{ Authorization = "token $token" }

# 读取美化的 Release Notes
$bodyPath = "C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\RELEASE_NOTES.md"
$body = Get-Content -Raw -Encoding UTF8 $bodyPath

# 更新 Release body（PATCH）
$jsonBody = @{ body = $body } | ConvertTo-Json
$r = Invoke-RestMethod -Uri "https://api.github.com/repos/leeymxz/playdl/releases/386213136" -Method PATCH -Headers $headers -Body $jsonBody -ContentType "application/json; charset=utf-8"
Write-Output "Release body updated: $($r.tag_name)"
Write-Output "DONE"