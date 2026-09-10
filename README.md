<p align="center">
  <img src="docs/logo.png" alt="PlayDL Logo" width="200"/>
</p>

# PlayDL - 高速下载管理器

**PlayDL** 是一款快速、弹性、多源的文件下载加速器，支持 Windows、macOS 和 Linux。

基于 Hydra Download Manager 核心引擎，提供卓越的下载性能和稳定性。

---

## ✨ 核心特性

### 引擎
- **自适应并发** — 自动分片下载，动态平衡连接
- **Range 窃取** — 从慢连接自动转移任务到快连接
- **卡死检测** — 统计算法提前发现劣化连接
- **多协议支持** — HTTP(S)、FTP、SOCKS4/4a/5 代理
- **完整性校验** — SHA-256/BLAKE3 + Reed-Solomon 纠错码
- **内存恒定** — 直接定位写入，内存占用不随文件大小增长

### CLI
- **简单易用** — `playdl <url>` 即可开始下载
- **断点续传** — 自动恢复未完成的下载
- **Metalink 支持** — 多镜像源 + 分片校验
- **兼容模式** — 支持 `wget`/`curl` 兼容语法

### GUI (即将推出)
- 跨平台桌面应用
- 浏览器集成扩展
- 队列与调度器

---

## 🚀 快速安装

### Windows (PowerShell)
```powershell
irm https://raw.githubusercontent.com/leeymxz/playdl/main/install.ps1 | iex
```

### macOS / Linux
```bash
curl -fsSL https://raw.githubusercontent.com/leeymxz/playdl/main/install.sh | bash
```

### 从源码编译
```bash
git clone https://github.com/leeymxz/playdl.git
cd playdl
cargo build --release
# 编译好的二进制在 target/release/playdl.exe
```

---

## 📖 使用示例

```bash
# 基础下载
playdl https://example.com/file.zip

# 指定输出文件名
playdl https://example.com/file.zip -o myfile.zip

# 多连接加速
playdl -x 8 https://example.com/largefile.iso

# 多镜像源下载
playdl https://mirror1.example.com/file.iso https://mirror2.example.com/file.iso

# 断点续传
playdl -c https://example.com/file.zip

# 查看下载进度 (TUI)
playdl interactive
```

---

## 📊 性能对比

PlayDL 核心引擎在同类工具中表现出色：

| 测试项 | PlayDL | aria2c | curl |
|--------|--------|--------|------|
| 8 连接速度 | **414 MB/s** | 298 MB/s | - |
| 峰值内存 | **6.9 MiB** | 25.3 MiB | 10.9 MiB |
| CPU 时间 | **1.48 s** | 2.52 s | 3.21 s |

---

## 📝 许可证

- CLI 二进制: **GPL-3.0-or-later**
- 核心库 (pdl-core, pdl-net): **MIT OR Apache-2.0**

---

## 🙏 致谢

本项目基于 [ja7ad/hydra](https://github.com/ja7ad/hydra) 改造，感谢原作者的卓越工作。