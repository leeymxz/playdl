import os
import re
import sys

sys.stdout.reconfigure(encoding="utf-8")
base = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\crates\hydra-net\src"

for root, dirs, files in os.walk(base):
    for f in files:
        if not f.endswith(".rs"):
            continue
        p = os.path.join(root, f)
        text = open(p, encoding="utf-8").read()
        for m in re.finditer(r".{0,50}\b(min|max|clamp|limit).{0,10}\(\s*(\d+)", text):
            if int(m.group(2)) <= 32:
                print(f"{os.path.relpath(p,base)}: {m.group(0).replace(chr(10),' ')}")