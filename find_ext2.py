import os
import sys

sys.stdout.reconfigure(encoding="utf-8")
p = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\crates\hydra-gui\src\extbus.rs"
text = open(p, encoding="utf-8").read()
lines = text.splitlines()
print(f"extbus.rs: {len(lines)} lines")

# 找 type 分发
for i, line in enumerate(lines, 1):
    low = line.lower()
    if '"download"' in low or "msg.type" in low or "kind" in low and "match" in low:
        print(f"L{i}: {line.strip()[:110]}")
print("---")
# 找主分发逻辑
for i, line in enumerate(lines, 1):
    if "match" in line and ("type" in line or "kind" in line or "msg" in line):
        print(f"L{i}: {line.strip()[:110]}")