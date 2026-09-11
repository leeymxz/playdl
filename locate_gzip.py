import os
import re
import sys

sys.stdout.reconfigure(encoding="utf-8")

base = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\extensions"
hits = []

for root, dirs, files in os.walk(base):
    if ".git" in root:
        continue
    for f in files:
        if f.endswith((".js", ".html", ".json", ".css")):
            p = os.path.join(root, f)
            try:
                lines = open(p, encoding="utf-8").read().splitlines()
            except Exception:
                continue
            for i, line in enumerate(lines, 1):
                low = line.lower()
                if "gzip" in low:
                    hits.append((p, i, line.strip()))

# 找 .gzip 属性访问（真正的报错点）
print("=== .gzip 属性访问 ===")
for p, i, line in hits:
    if ".gzip" in line:
        print(f"  {os.path.relpath(p, base)}:{i}: {line[:120]}")

print()
print("=== 含 gzip 的字符串/注释（供参考）===")
for p, i, line in hits:
    if ".gzip" not in line:
        print(f"  {os.path.relpath(p, base)}:{i}: {line[:120]}")
print("DONE")