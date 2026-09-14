import os
import re
import sys

sys.stdout.reconfigure(encoding="utf-8")
base = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\crates"

for root, dirs, files in os.walk(base):
    for f in files:
        if not f.endswith(".rs"):
            continue
        p = os.path.join(root, f)
        text = open(p, encoding="utf-8").read()
        if "MAX_WORKERS" in text or "max_workers" in text:
            for m in re.finditer(r".{0,40}(MAX_WORKERS|max_workers).{0,60}", text):
                print(f"{os.path.relpath(p,base)}: {m.group(0).replace(chr(10),' ')}")