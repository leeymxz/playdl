$ErrorActionPreference = "Stop"
$enc = [System.Text.UTF8Encoding]::new($false)
$dir = "C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\crates\hydra-gui"
$files = Get-ChildItem -Recurse -Filter "*.rs" $dir
$fixed = 0

# 品牌替换表（按顺序）
$rules = @(
    @("Hydra Download Manager", "PlayDL Download Manager"),
    @("Hydra Configuration", "PlayDL 设置"),
    @("About Hydra", "关于 PlayDL"),
    @("Update Hydra", "更新 PlayDL"),
    @("Hydra Home Page", "PlayDL 主页"),
    @("hydra-gui", "playdl-gui"),
    @("Hydra", "PlayDL"),
    @("hydra", "playdl")
)

foreach ($f in $files) {
    $text = [System.IO.File]::ReadAllText($f.FullName, [System.Text.Encoding]::UTF8)
    $orig = $text
    foreach ($r in $rules) {
        $text = $text.Replace($r[0], $r[1])
    }
    if ($text -ne $orig) {
        [System.IO.File]::WriteAllText($f.FullName, $text, $enc)
        $fixed++
    }
}

# build.rs 的图标路径也要改
$br = "$dir\build.rs"
if (Test-Path $br) {
    $t = [System.IO.File]::ReadAllText($br, [System.Text.Encoding]::UTF8)
    $o = $t
    $t = $t.Replace("scripts/windows/hydra.ico", "docs/playdl.ico")
    $t = $t.Replace("hydra-gui.rc", "playdl-gui.rc")
    if ($t -ne $o) {
        [System.IO.File]::WriteAllText($br, $t, $enc)
        Write-Output "Fixed build.rs"
    }
}

Write-Output "共修改 $fixed 个 .rs 文件"
