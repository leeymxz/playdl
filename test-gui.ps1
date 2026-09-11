$ErrorActionPreference = "Continue"
$dir = "C:\Users\Administrator\Documents\Loomy Workspace\my-downloader"
$exe = "$dir\target\release\playdl-gui.exe"

$p = Start-Process -FilePath $exe -WorkingDirectory $dir -PassThru
Start-Sleep -Seconds 4

if ($p.HasExited) {
    Write-Output "EXITED code=$($p.ExitCode)"
} else {
    Write-Output "RUNNING pid=$($p.Id)"
    # 找监听端口
    $listeners = netstat -ano | Select-String $p.Id
    foreach ($l in $listeners) { Write-Output "LISTEN: $l" }
    # 测试 127.0.0.1 上是否有 HTTP 服务（遍历几个端口）
    foreach ($port in 49152..49200) {
        try {
            $r = Invoke-WebRequest -Uri "http://127.0.0.1:$port/api/engine" -UseBasicParsing -TimeoutSec 2 -ErrorAction Stop
            Write-Output "FOUND port $port -> $($r.Content)"
            break
        } catch {}
    }
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    Write-Output "STOPPED"
}