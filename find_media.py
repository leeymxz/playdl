import sys
import re

sys.stdout.reconfigure(encoding="utf-8")
text = open(
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\extensions\chrome\content.js",
    encoding="utf-8",
).read()

for kw in ["dedupe", "unique", "Set(", "distinct", "seen", "pageItems.media", "media =", "media="]:
    idx = text.find(kw)
    if idx >= 0:
        print(f"[{kw}] @ 行 {text[:idx].count(chr(10))+1}")
        print(f"  {text[max(0,idx-40):idx+120].replace(chr(10),' ')}")
        print()

# 找 pageItems.media 赋值处
idx = text.find("media")
for m in re.finditer(r"pageItems\.media\s*=|media\s*=\s*\[", text):
    s = max(0, m.start() - 60)
    print(f"media赋值 @ 行 {text[:m.start()].count(chr(10))+1}: ...{text[s:m.start()+200].replace(chr(10),' ')}...")
    print()
print("DONE")