import os
import re
import sys

sys.stdout.reconfigure(encoding="utf-8")
base = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\crates\hydra-gui\src"

for root, dirs, files in os.walk(base):
    for f in files:
        if not f.endswith(".rs"):
            continue
        p = os.path.join(root, f)
        text = open(p, encoding="utf-8").read()
        for m in re.finditer(r".{0,60}(check_updates_on_startup|checkOnStartup|updates_on_startup|update_check|UpdateCheck|CheckUpdates).{0,70}", text):
            print(f"{os.path.relpath(p,base)}:L{text[:m.start()].count(chr(10))+1}: {m.group(0).replace(chr(10),' ')}")