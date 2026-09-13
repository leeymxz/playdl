import sys
import re

sys.stdout.reconfigure(encoding="utf-8")
text = open(
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\crates\hydra-gui\src\app.rs",
    encoding="utf-8",
).read()

for m in re.finditer(r"capture_raise", text):
    line_no = text[: m.start()].count("\n") + 1
    s = max(0, m.start() - 40)
    snippet = text[s : m.start() + 90].replace("\n", " ")
    print(f"L{line_no}: ...{snippet}...")
print("DONE")