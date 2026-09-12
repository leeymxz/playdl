$ErrorActionPreference = "Stop"
$token = $env:GITHUB_TOKEN
if (-not $token) { Write-Output "ERROR: set GITHUB_TOKEN first"; exit 1 }
$headers = @{ Authorization = "token $token" }
$dir = "C:\Users\Administrator\Documents\Loomy Workspace\my-downloader"
$relId = 387628889  # v0.3.0 release

$uploads = @(
    @{ File = "$dir\packaging\dist\PlayDL-Setup-0.3.0.exe"; Name = "PlayDL-Setup-0.3.0.exe" },
    @{ File = "$dir\dist\playdl-extension-chrome.zip"; Name = "playdl-extension-chrome.zip" },
    @{ File = "$dir\dist\playdl-extension-firefox.zip"; Name = "playdl-extension-firefox.zip" }
)

foreach ($u in $uploads) {
    if (-not (Test-Path $u.File)) { Write-Output "SKIP: $($u.Name)"; continue }
    $cur = Invoke-RestMethod -Uri "https://api.github.com/repos/leeymxz/playdl/releases/$relId" -Headers $headers
    $old = $cur.assets | Where-Object { $_.name -eq $u.Name }
    if ($old) {
        Invoke-RestMethod -Uri $old.url -Method DELETE -Headers $headers
        Write-Output "Deleted old: $($u.Name)"
    }
    $url = "https://uploads.github.com/repos/leeymxz/playdl/releases/$relId/assets?name=$($u.Name)"
    $h = @{ Authorization = "token $token"; "Content-Type" = "application/octet-stream" }
    $up = Invoke-RestMethod -Uri $url -Method POST -Headers $h -InFile $u.File
    $sizeKb = [math]::Round($up.size / 1KB, 0)
    Write-Output "Uploaded: $($u.Name) ($sizeKb KB)"
}
Write-Output "DONE"