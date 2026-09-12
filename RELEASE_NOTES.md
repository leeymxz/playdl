# ⚡ PlayDL v0.2.0

<p align="center">
  <b>多源高速下载加速器 · 自适应并发 · 断点续传 · 多源合并</b>
</p>

---

## 📦 下载文件

### 🪟 Windows

| 文件 | 大小 | 说明 |
|------|------|------|
| [**PlayDL-Setup-0.2.0.exe**](https://github.com/leeymxz/playdl/releases/download/v0.2.0/PlayDL-Setup-0.2.0.exe) | 11.5 MB | **推荐** · 安装包（含 GUI + 浏览器扩展） |
| [playdl-0.2.0-x86_64-pc-windows-msvc.zip](https://github.com/leeymxz/playdl/releases/download/v0.2.0/playdl-0.2.0-x86_64-pc-windows-msvc.zip) | 8.2 MB | 绿色免安装版（解压即用） |

### 🐧 Linux

| 文件 | 大小 | 说明 |
|------|------|------|
| [playdl-0.2.0-x86_64-unknown-linux-gnu.tar.gz](https://github.com/leeymxz/playdl/releases/download/v0.2.0/playdl-0.2.0-x86_64-unknown-linux-gnu.tar.gz) | 7.8 MB | x86_64 (Intel/AMD) |

### 🍎 macOS

| 文件 | 大小 | 说明 |
|------|------|------|
| [playdl-0.2.0-x86_64-apple-darwin.tar.gz](https://github.com/leeymxz/playdl/releases/download/v0.2.0/playdl-0.2.0-x86_64-apple-darwin.tar.gz) | 7.6 MB | Intel 芯片 |
| [playdl-0.2.0-aarch64-apple-darwin.tar.gz](https://github.com/leeymxz/playdl/releases/download/v0.2.0/playdl-0.2.0-aarch64-apple-darwin.tar.gz) | 7.1 MB | Apple Silicon (M系列) |

### 🌐 浏览器扩展

| 文件 | 大小 | 说明 |
|------|------|------|
| [playdl-extension-chrome.zip](https://github.com/leeymxz/playdl/releases/download/v0.2.0/playdl-extension-chrome.zip) | 67 KB | Chrome / Edge / 遨游 / Chromium 系 |
| [playdl-extension-firefox.zip](https://github.com/leeymxz/playdl/releases/download/v0.2.0/playdl-extension-firefox.zip) | 65 KB | Firefox |
| [安装指南](https://github.com/leeymxz/playdl/releases/download/v0.2.0/PlayDL-browser-extension-guide.md) | 2 KB | 扩展安装图文教程 |

---

## 🚀 快速安装

### Windows（管理员 PowerShell）
```powershell
irm https://raw.githubusercontent.com/leeymxz/playdl/main/install.ps1 | iex
```

### macOS / Linux
```bash
curl -fsSL https://raw.githubusercontent.com/leeymxz/playdl/main/install.sh | bash
```

---

## 📖 使用示例

```bash
playdl https://example.com/file.zip          # 基本下载
pdl https://example.com/file.zip             # 缩写命令
playdl -x 8 https://example.com/large.iso    # 8 连接加速
playdl --limit-rate 2M https://example.com/f # 限速下载
playdl interactive                           # TUI 交互界面
playdl --json https://example.com/f          # 脚本友好输出
```

---

## ✨ 特性

- 🧠 自适应并发调度（比 aria2c 快 40%，内存仅 1/3）
- 🔄 Range Stealing + 卡死连接检测
- ✅ SHA-256/BLAKE3 + Reed-Solomon 完整性校验
- 📋 Metalink / HLS / DASH 多协议支持
- 🌐 SOCKS4/4a/5 代理
- 🖥️ 完整 GUI（IDM 风格）+ 浏览器扩展

## 📝 许可证

| 组件 | 许可证 |
|------|--------|
| CLI / GUI | GPL-3.0-or-later |
| 核心库 (pdl-core, pdl-net) | MIT OR Apache-2.0 |

---

<p align="center">
  <a href="https://github.com/leeymxz/playdl">🌐 GitHub</a> ·
  <a href="https://github.com/leeymxz/playdl/issues">🐛 报告问题</a> ·
  © 2026 leeymxz
</p>
