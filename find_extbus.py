import os
import sys

sys.stdout.reconfigure(encoding="utf-8")
base = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\crates\hydra-gui\src"

# 找 extbus 文件
for f in os.listdir(base):
    if "extbus" in f.lower() or "ext" in f.lower():
        print(f"file: {f}")

# 在可能的文件里找 download 类型处理
for f in os.listdir(base):
    if not f.endswith(".rs"):
        continue
    p = os.path.join(base, f)
    text = open(p, encoding="utf-8").read()
    if '"download"' in text or '"download' in text:
        idx = text.find('"download"')
        if idx < 0:
            idx = text.find('"download')
        print(f"\n=== {f} ===")
        print(text[max(0, idx - 100):idx + 300])
        break