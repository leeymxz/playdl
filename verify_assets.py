import sys

sys.stdout.reconfigure(encoding="utf-8")

# 模拟 updater 的 Exe 匹配逻辑: starts_with('PlayDL-') && ends_with('-setup.exe') && contains('x64')
assets = [
    "playdl-0.3.0-x86_64-pc-windows-msvc.zip",
    "playdl-0.3.0-x86_64-unknown-linux-gnu.tar.gz",
    "PlayDL-0.3.0-windows-x64-setup.exe",
    "PlayDL-Setup-0.3.0.exe",
]
for a in assets:
    match = a.startswith("PlayDL-") and a.endswith("-setup.exe") and "x64" in a
    print(f"{a}: {'MATCH' if match else 'no'}")

print()
print("GUI 资产名 (新 release.yml 会生成):")
print("  playdl-0.3.0-windows-amd64.zip  <- updater 期望")
print("  playdl-0.3.0-linux-amd64.tar.gz")
print("  playdl-0.3.0-macos-amd64.tar.gz")
print("  playdl-0.3.0-macos-arm64.tar.gz")