import os
import re
import sys

sys.stdout.reconfigure(encoding="utf-8")

base = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\crates\hydra-gui\src"

# 在 app.rs 找 CheckUpdates 相关和启动触发
for fname in ["app.rs", "main.rs"]:
    p = os.path.join(base, fname)
    text = open(p, encoding="utf-8").read()
    lines = text.splitlines()
    print(f"=== {fname} ===")
    for i, line in enumerate(lines, 1):
        low = line.lower()
        if "checkupdates" in low or "startup" in low and "update" in low:
            print(f"  L{i}: {line.strip()[:110]}")
    print()