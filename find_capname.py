import sys
import re

sys.stdout.reconfigure(encoding="utf-8")
text = open(
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\crates\hydra-gui\src\app.rs",
    encoding="utf-8",
).read()

for m in re.finditer(r"capture_name", text):
    s = max(0, m.start() - 60)
    snippet = text[s : m.start() + 110].replace("\n", " ")
    print(f"L{text[:m.start()].count(chr(10))+1}: ...{snippet}...")
    print()
print("DONE")