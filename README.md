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

### 🎬 视频号下载（`wxchannel` / `wx`）

> **这是「下载侧」的能力。** 视频号真正播放地址是**短时效的签名链接**（形如
> `https://finder.video.qq.com/…/stodownload?encfilekey=…&token=…&sign=…&svrnonce=…`），
> 由微信自己的私有协议在已登录会话里下发。PlayDL **不去复现这一步**，也不去解视频头部的掩码；
> 它负责的是后半段：你已经有链接了，让它用满速多连接把文件稳稳拉下来。

```bash
# 1) 直接粘一条链接
playdl wxchannel 'https://finder.video.qq.com/251/20302/stodownload?encfilekey=…&token=…'

# 2) 批量：吃常见采集工具导出的清单
playdl wxchannel items.json -d 视频号 -x 16 --by-author --cover

# 3) 先看看清单里有什么，不下
playdl wxchannel items.json --list
```

| 选项 | 作用 |
|------|------|
| `-d, --dir <DIR>` | 保存到指定目录 |
| `-x, --conns <N>` | 每文件连接数，默认 `8`（腾讯 CDN 支持 Range，多连接明显更快） |
| `--by-author` | 按作者分目录 |
| `--cover` | 顺带下载封面图 |
| `--no-referer` | 不发送 `Referer`（个别 CDN 节点更严格时可试） |
| `--json` | 机器可读输出 |
| `--list` | 只列清单 |
| `--template <TPL>` | 文件名模板，占位符 `{author} {title} {id} {res} {dur} {date} {index}`，`/` 分目录；默认 `作者 - 标题` |
| `-j, --jobs <N>` | 同时下载的条目数，默认 `1`（`>1` 时隐藏进度条，结束后按清单顺序回放每条结果，避免交错） |
| `--retries <N>` | 单条失败后额外重试次数，默认 `1`（重试历史会一并回放） |
| `--record <FILE>` | 下载台账路径（默认 `<DIR>/.playdl-wxchannel.jsonl`），用于跨会话去重 |
| `--no-record` | 本次不写台账 |
| `--force` | 即使台账已记录为完成也重新下载 |
| `--export <FILE>` | 把本次结果导出成清单：`.csv` 为带 BOM 的表格，其它后缀为 JSON |
| `--from <KIND>` | 强制输入格式 `auto/json/csv/har`；`auto` 自动识别 HAR 抓包、CSV 表格、JSON 数组、JSON Lines 与每行一个 URL |

**它替你做的几件事**

- **文件名自动整理**：`作者 - 标题.mp4`。标题里常见的换行、`#话题#`、以及 Windows 不允许的
  `/ \ : * ? " < > |` 全部清理干净，长标题会被截断。只有链接没有标题时，用链接里的
  `encfilekey` 生成不重复的名字（`wxchannel-Cvvj5Ix3eez3Y79S.mp4`），避免两条视频互相覆盖。
- **时长单位自动换算**：清单里的 `duration` 可能是秒也可能是毫秒，按文件大小反算码率来判断；
  `size` / `duration_ms` / `durationMs` 也都认。
- **失败说人话**：链接过期给「腾讯返回链接已过期（需重新获取）」，而不是一句 HTTP 状态码；
  链接里的 `svrnonce` 是签发时间，超过一天会先提示你这条大概率已经不能用了。
- **断点续传 / 重复跳过**：中断后重跑继续传；已有同大小文件直接跳过。
- **受限内容明确标注**：微信对部分视频加了头部掩码，PlayDL **不会**去还原它，而是原样保存并
  重命名为 `xxx.masked.mp4`，让你一眼知道这个文件的播放器只有微信自己。

**清单支持四种写法**（都能直接喂进来）

```jsonc
// 数组、{"items":[…]}、每行一个 JSON（JSON Lines）、或每行一个 URL（# 开头为注释）
[
  {
    "title": "视频标题",
    "author": "作者名",
    "video_url": "https://finder.video.qq.com/251/20302/stodownload?encfilekey=…",
    "cover_url": "https://…/cover.jpg",
    "size": 207588679,
    "duration": 266834,
    "resolution": "1080x1920"
  }
]
```
字段别名也做了兼容：标题认 `title/desc/description/name`，作者认 `author/nickname/author_name/creator`，
地址认 `video_url/url/link/media_url/play_url`。

**抓包导入：HAR 与 CSV（推荐拿到链接的方式）**

拿链接这一步 PlayDL 不做，也不该做——不去注入任何客户端、不装证书。规矩的做法是**用你自己的抓包工具**
（mitmproxy / Fiddler / Charles）看自己的流量，导成文件再喂进来：

- **HAR**：抓包工具导出的 `*.har`（`--from har`）。自动挑出 `finder.video.qq.com` 的
  `stodownload` 请求与带 `encfilekey` 的媒体响应，按 URL 去重，并从响应体里捞标题、作者、时长。
- **CSV**：带表头的表格（`--from csv`），表头认中文与英文别名——
  `标题/author/作者/链接/url/link/时长/duration/大小/size/分辨率/resolution/编号/index`；
  没有表头就当「每行一个 URL」处理。逗号、引号、CRLF 都按 RFC 4180 解析。

```bash
# 抓到自己的流量 → 导出 HAR → 直接喂
playdl wxchannel my_capture.har -d 视频号 -x 16 --template '{author}/{title}' -j 4

# 或导成 CSV 台账后再批量下载
playdl wxchannel export.csv -d 视频号 --from csv -j 4 --retries 2 --export 视频号/台账.csv
```

导出的 `台账.csv` 带 UTF-8 BOM、字段用引号包裹，Excel 直接双击就能打开；`--record` 指向的 JSONL
台账让第二次运行自动跳过已经下好的条目（用签名链接或 ID 做身份），`--force` 可强制重下。

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
| 🎬 **视频号** | `wxchannel` 子命令：签名链接多连接下载、文件名整理、受限内容标注 |
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