import sys
import re

sys.stdout.reconfigure(encoding="utf-8")
text = open(
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\extensions\chrome\content.js",
    encoding="utf-8",
).read()

for m in re.finditer(r"label:", text):
    s = max(0, m.start() - 180)
    line = text[s : m.start() + 150].replace("\n", " ")
    print(f"@ 行 {text[:m.start()].count(chr(10))+1}: ...{line}...")
    print()
print("DONE")