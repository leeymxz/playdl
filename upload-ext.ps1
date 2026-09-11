$ErrorActionPreference = "Stop"
$token = $env:GITHUB_TOKEN
if (-not $token) {
    Write-Output "ERROR: set GITHUB_TOKEN env var first"
    exit 1
}
$dir = "C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\dist"
$relId = 386213136
$headers = @{ Authorization = "token $token" }

$uploads = @(
    @{ File = "$dir\playdl-extension-chrome.zip"; Name = "playdl-extension-chrome.zip" },
    @{ File = "$dir\playdl-extension-firefox.zip"; Name = "playdl-extension-firefox.zip" },
    @{ File = "$dir\PlayDL浏览器扩展-安装指南.md"; Name = "PlayDL-browser-extension-guide.md" }
)

foreach ($u in $uploads) {
    if (-not (Test-Path $u.File)) {
        Write-Output "SKIP (missing): $($u.Name)"
        continue
    }
    $asset = Invoke-RestMethod -Uri "https://api.github.com/repos/leeymxz/playdl/releases/386213136" -Headers $headers
    $old = $asset.assets | Where-Object { $_.name -eq $u.Name }
    if ($old) {
        Invoke-RestMethod -Uri $old.url -Method DELETE -Headers $headers
        Write-Output "Deleted old: $($u.Name)"
    }
    $uploadUrl = "https://uploads.github.com/repos/leeymxz/playdl/releases/$relId/assets?name=$($u.Name)"
    $upHeaders = @{
        Authorization = "token $token"
        "Content-Type" = "application/octet-stream"
    }
    $up = Invoke-RestMethod -Uri $uploadUrl -Method POST -Headers $upHeaders -InFile $u.File
    Write-Output "Uploaded: $($u.Name) ($($up.size) bytes)"
}
Write-Output "DONE"