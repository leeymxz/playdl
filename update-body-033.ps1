$ErrorActionPreference = "Stop"
$token = $env:GITHUB_TOKEN
if (-not $token) { Write-Output "ERROR: set GITHUB_TOKEN first"; exit 1 }
$headers = @{ Authorization = "token $token" }

# 读取更新说明
$bodyPath = "C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\RELEASE_NOTES_033.md"
$body = Get-Content -Raw -Encoding UTF8 $bodyPath

# 更新 v0.3.3 release body
$jsonBody = @{ body = $body } | ConvertTo-Json
$r = Invoke-RestMethod -Uri "https://api.github.com/repos/leeymxz/playdl/releases/388106907" -Method PATCH -Headers $headers -Body $jsonBody -ContentType "application/json; charset=utf-8"
Write-Output "Release body updated for $($r.tag_name)"
Write-Output "DONE"