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
        for m in re.finditer(r".{0,50}(clamp\(1, 32\)|\.min\(32\)|MAX_CONN|max_conns\s*=\s*32|= 32\b).{0,40}", text):
            print(f"{os.path.relpath(p,base)}:L{text[:m.start()].count(chr(10))+1}: {m.group(0).replace(chr(10),' ')}")