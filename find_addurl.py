import sys
import re

sys.stdout.reconfigure(encoding="utf-8")
text = open(
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\crates\hydra-gui\src\app.rs",
    encoding="utf-8",
).read()

idx = text.find("Message::AddUrlOk")
if idx < 0:
    idx = text.find("AddUrlOk =>")
print(text[idx : idx + 900])