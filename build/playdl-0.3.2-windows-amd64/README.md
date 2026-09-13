<p align="center">
  <img src="docs/logo.png" alt="PlayDL Logo" width="250"/>
</p>

<h1 align="center">PlayDL ⚡</h1>

<p align="center">
  <b>高速多源下载加速器 — Multi-Source Download Accelerator</b>
</p>

<p align="center">
  <a href="https://github.com/leeymxz/playdl/actions/workflows/ci.yml">
    <img src="https://github.com/leeymxz/playdl/actions/workflows/ci.yml/badge.svg" alt="CI Status"/>
  </a>
  <a href="https://github.com/leeymxz/playdl/actions/workflows/release.yml">
    <img src="https://github.com/leeymxz/playdl/actions/workflows/release.yml/badge.svg" alt="Release Status"/>
  </a>
  <img src="https://img.shields.io/badge/Rust-1.98+-orange.svg" alt="Rust Version"/>
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-blue.svg" alt="Platform"/>
  <img src="https://img.shields.io/badge/license-MIT%2FApache--2.0%20%7C%20GPL--3.0-green.svg" alt="License"/>
</p>

<pre align="center">
   ██████╗ ██╗      █████╗ ██╗   ██╗██████╗ ██╗
   ██╔══██╗██║     ██╔══██╗╚██╗ ██╔╝██╔══██╗██║
   ██████╔╝██║     ███████║ ╚████╔╝ ██║  ██║██║
   ██╔═══╝ ██║     ██╔══██║  ╚██╔╝  ██║  ██║╚═╝
   ██║     ███████╗██║  ██║   ██║   ██████╔╝██╗
   ╚═╝     ╚══════╝╚═╝  ╚═╝   ╚═╝   ╚═════╝ ╚═╝
</pre>

<p align="center">
  <code>playdl https://example.com/file.zip</code> &nbsp;·&nbsp; <code>pdl https://example.com/file.zip</code>
</p>

<p align="center">
  <i>比 aria2c 快 40%，内存仅 1/3，原生 Windows Terminal 体验</i>
</p>

---

## ✨ 亮点速览

```text
$ playdl https://cdn.example.com/ubuntu-24.04-desktop-amd64.iso

 PlayDL ⚡ file retriever      1 running  0 queued  0 done  0 failed  |  24.4 MB/s  |  max 3
 ─────────────────────────────────────────────────────────────────────────────────────────────
 run  ubuntu-24.04-desktop-amd64.iso  ████████░░░░░░░░░░░░░░  48.2%  2.9 GB/6.0 GB  24.4 MB/s
 ─────────────────────────────────────────────────────────────────────────────────────────────
  start #1 ubuntu-24.04-desktop-amd64.iso
```

---

## 🚀 快速安装

### Windows (一行命令)
```powershell
# 管理员 PowerShell
irm https://raw.githubusercontent.com/leeymxz/playdl/main/install.ps1 | iex
```

### macOS / Linux (一行命令)
```bash
curl -fsSL https://raw.githubusercontent.com/leeymxz/playdl/main/install.sh | bash
```

### 从源码编译
```bash
git clone https://github.com/leeymxz/playdl.git
cd playdl
cargo build --release
# 编译产物：
#   target/release/playdl.exe  (主程序)
#   target/release/pdl.exe     (缩写别名)
```

### Windows 安装包
下载 [PlayDL-Setup-*.exe](https://github.com/leeymxz/playdl/releases) → 双击安装

---

## 📖 使用指南

### 基础用法
```bash
playdl https://example.com/file.zip              # 一键下载
pdl https://example.com/file.zip                 # 缩写也一样
playdl -o myfile.zip https://example.com/f.zip   # 指定文件名
```

### 加速下载
```bash
playdl -x 8 https://example.com/large-file.iso   # 8 连接并发
playdl --limit-rate 2M https://example.com/f      # 限速 2MB/s
playdl --polite -x 16 https://example.com/f       # 温和模式
```

### 多源与校验
```bash
# 多镜像源（从最快源拼文件）
playdl https://mirror1.example.com/file.iso https://mirror2.example.com/file.iso

# MetaLink 格式
playdl --metalink list.meta4

# 断点续传 + SHA-256 校验
playdl -c --checksum sha256:abc123... https://example.com/f
```

### 交互模式
```bash
playdl interactive
```
启动 TUI 交互式下载管理器，支持：
- 实时进度条 + 速率图
- 队列管理（暂停/恢复/重排）
- 单连接详情面板
- 快捷键：`a` 添加 `p/r` 暂停/恢复 `d` 取消 `?` 帮助

### JSON 输出（脚本友好）
```bash
playdl --json https://example.com/f
```

---

## 🔧 核心技术

| 特性 | 说明 |
|------|------|
| 🧠 **自适应并发** | 自动分片并动态平衡连接数，不浪费带宽 |
| 🔄 **Range Stealing** | 慢连接的剩余范围自动转移给快连接 |
| ⏱️ **卡死检测** | 统计算法提前发现劣化连接，不等超时 |
| ✅ **完整性校验** | SHA-256 / BLAKE3 + Reed-Solomon 纠错码 |
| 💾 **内存恒定** | 直接定位写入，文件多大都不爆内存 |
| 🌐 **多协议** | HTTP(S) / FTP / Metalink / HLS / DASH |
| 🔌 **SOCKS 代理** | SOCKS4 / SOCKS4a / SOCKS5 |
| 🪶 **极低内存** | 8 连接仅 6.9 MiB，不到 aria2c 的 1/3 |

---

## 📊 性能

| 测试 | PlayDL | aria2c | curl |
|------|--------|--------|------|
| 8 连接下载 (100MB) | **2.8s** | 3.9s | — |
| 峰值内存占用 | **6.9 MiB** | 25.3 MiB | 10.9 MiB |
| CPU 用户时间 | **1.48s** | 2.52s | 3.21s |

*测试环境：1 Gbps 链路，100 MB 随机文件，8 连接*

---

## 🏗️ 项目结构

```
playdl/
├── crates/
│   ├── pdl-core/        # 🧠 无 I/O 调度引擎 (MIT)
│   ├── pdl-net/         # 🌐 HTTP/1.1 传输层 (MIT)
│   ├── pdl-stream/      # 📺 HLS/DASH 流媒体
│   ├── pdl-ffi/         # 🔗 C ABI 接口 libplaydl (MIT)
│   ├── pdl-cli/         # ⌨️ CLI 命令行工具
│   ├── pdl-gui/         # 🖥️ 桌面 GUI 应用
│   ├── pdl-host/        # 🌍 浏览器原生消息桥
│   └── pdl-updater/     # 🔄 自更新机制
├── gui/                 # 🖱️ 双击启动的 Windows GUI
├── packaging/           # 📦 Windows 安装包脚本
└── docs/                # 📝 Logo 与文档
```

---

## 📝 许可证

| 组件 | 许可证 |
|------|--------|
| CLI / GUI 二进制 | **GPL-3.0-or-later** |
| 核心库 (pdl-core, pdl-net) | **MIT OR Apache-2.0** |
| C ABI (libplaydl) | **MIT OR Apache-2.0** |

核心引擎使用宽松许可证（MIT/Apache-2.0），可以放心嵌入到你的项目中。

---

## 🙏 致谢

本项目基于 [ja7ad/hydra](https://github.com/ja7ad/hydra) 改造而来，感谢原作者的卓越工作。

---

<p align="center">
  <a href="https://github.com/leeymxz/playdl">GitHub</a> ·
  <a href="https://github.com/leeymxz/playdl/releases">Releases</a> ·
  <a href="https://github.com/leeymxz/playdl/issues">Issues</a>
</p>