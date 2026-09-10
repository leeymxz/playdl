# PlayDL {{ tag_name }}

## 下载

| 平台 | 架构 | 格式 |
|------|------|------|
| Windows | x86_64 | .zip |
| Linux | x86_64 | .tar.gz |
| Linux | ARM64 | .tar.gz |
| macOS | x86_64 | .tar.gz |
| macOS | ARM64 (Apple Silicon) | .tar.gz |

## 安装

**Windows** （管理员 PowerShell）：
```powershell
irm https://raw.githubusercontent.com/leeymxz/playdl/main/install.ps1 | iex
```

**Linux / macOS**：
```bash
curl -fsSL https://raw.githubusercontent.com/leeymxz/playdl/main/install.sh | bash
```

## 使用

```bash
playdl https://example.com/file.zip          # 基本下载
pdl https://example.com/file.zip             # 缩写
playdl -x 8 https://example.com/large.iso    # 8 连接加速
playdl interactive                           # TUI 交互模式
```

## 更新日志

- 见 [CHANGELOG.md](https://github.com/leeymxz/playdl/blob/main/CHANGELOG.md)