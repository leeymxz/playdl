import sys
import re

sys.stdout.reconfigure(encoding="utf-8")
text = open(
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\extensions\chrome\background.js",
    encoding="utf-8",
).read()

# 找 onMessage 监听和消息分发
idx = text.find("onMessage")
if idx > 0:
    seg = text[idx : idx + 4000]
    types = set()
    for m in re.finditer(r'type[:\s]*["\'](\w+)["\']', seg):
        types.add(m.group(1))
    for m in re.finditer(r'["\'](\w+)["\']\s*[=:]', seg):
        t = m.group(1)
        if t in {"video", "download", "stream", "download-url", "download-stream", "open-playdl", "get-state", "ping", "config"}:
            types.add(t)
    print("处理的消息类型:")
    for t in sorted(types):
        print(f"  {t}")

# 看 onMessage 主分发
m = re.search(r'chrome\.runtime\.onMessage[^}]{0,200}', text)
if m:
    print("\n--- onMessage 开头 ---")
    print(m.group(0)[:300])