import sys
import re

sys.stdout.reconfigure(encoding="utf-8")
text = open(
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\crates\hydra-gui\src\update.rs",
    encoding="utf-8",
).read()

for kw in ["updater", "playdl-updater", "current_exe"]:
    for m in re.finditer(kw, text):
        s = max(0, m.start() - 60)
        print(f"[{kw}] L{text[:m.start()].count(chr(10))+1}: {text[s:m.start()+90].replace(chr(10),' ')}")
        print()