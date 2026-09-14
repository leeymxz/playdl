# ⚡ PlayDL {{ tag_name }}

<p align="center">
  <b>多源高速下载加速器 · 全平台视频下载 · 浏览器悬停即下</b>
</p>

---

## 📝 更新内容

> 本次版本的更新说明。发布时请在这里补充本次更新/修复的内容：
> - ✨ 新功能
> - 🐛 修复
> - 🎨 改进

- （待补充更新说明）

---

## 📦 下载文件

### 🪟 Windows

| 文件 | 说明 |
|------|------|
| [PlayDL-{{ version }}-windows-x64-setup.exe](https://github.com/leeymxz/playdl/releases/download/{{ tag_name }}/PlayDL-{{ version }}-windows-x64-setup.exe) | **推荐** · 安装包（含 GUI + 浏览器扩展 + yt-dlp） |
| [playdl-{{ version }}-windows-amd64.zip](https://github.com/leeymxz/playdl/releases/download/{{ tag_name }}/playdl-{{ version }}-windows-amd64.zip) | 绿色免安装版（解压即用） |

### 🐧 Linux

| 文件 | 说明 |
|------|------|
| [playdl-{{ version }}-linux-amd64.tar.gz](https://github.com/leeymxz/playdl/releases/download/{{ tag_name }}/playdl-{{ version }}-linux-amd64.tar.gz) | Linux x86_64 |

### 🍎 macOS

| 文件 | 说明 |
|------|------|
| [playdl-{{ version }}-macos-amd64.tar.gz](https://github.com/leeymxz/playdl/releases/download/{{ tag_name }}/playdl-{{ version }}-macos-amd64.tar.gz) | Intel 芯片 |
| [playdl-{{ version }}-macos-arm64.tar.gz](https://github.com/leeymxz/playdl/releases/download/{{ tag_name }}/playdl-{{ version }}-macos-arm64.tar.gz) | Apple Silicon (M系列) |

### 🌐 浏览器扩展

| 文件 | 说明 |
|------|------|
| [playdl-extension-chrome.zip](https://github.com/leeymxz/playdl/releases/download/{{ tag_name }}/playdl-extension-chrome.zip) | Chrome / Edge / 遨游 / Chromium 系 |
| [playdl-extension-firefox.zip](https://github.com/leeymxz/playdl/releases/download/{{ tag_name }}/playdl-extension-firefox.zip) | Firefox |
| [PlayDL-browser-extension-guide.md](https://github.com/leeymxz/playdl/releases/download/{{ tag_name }}/PlayDL-browser-extension-guide.md) | 扩展安装教程 |

---

## 🚀 快速使用

```bash
playdl https://example.com/file.zip                    # 高速下载
playdl -x 16 https://example.com/large.iso            # 16 连接加速
playdl video "https://youtube.com/watch?v=xxx"        # 下载视频（1000+ 网站）
playdl video "https://www.bilibili.com/video/BVxxx"   # B站视频
playdl video "https://finder.video.qq.com/..."        # 微信视频号直链
playdl interactive                                    # TUI 交互界面
```

---

## ✨ 核心功能

- 🧠 自适应并发调度（比 aria2c 快 40%，内存仅 1/3）
- 🎬 支持 YouTube / B站 / 抖音 / TikTok / 视频号等 1000+ 视频网站
- 🖥️ 完整 IDM 风格 GUI + 浏览器扩展（悬停视频即下）
- 🔄 断点续传 · 多源合并 · H.264 兼容
- 🌐 全平台（Windows / Linux / macOS）

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
