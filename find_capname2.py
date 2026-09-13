import sys
import re

sys.stdout.reconfigure(encoding="utf-8")
text = open(
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\crates\hydra-gui\src\app.rs",
    encoding="utf-8",
).read()

for m in re.finditer(r"cap_name", text):
    print(f"L{text[:m.start()].count(chr(10))+1}: {text[m.start():m.start()+100].replace(chr(10),' ')}")
print("DONE")