import sys
import re

sys.stdout.reconfigure(encoding="utf-8")

# GUI extbus 处理的消息类型
text = open(
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\crates\hydra-gui\src\extbus.rs",
    encoding="utf-8",
).read()
types = set()
for m in re.finditer(r'Some\("(\w+)"\)', text):
    types.add(m.group(1))
print("GUI extbus 支持:", sorted(types))
print()

# 扩展发送的消息类型
ext = open(
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\extensions\chrome\background.js",
    encoding="utf-8",
).read()
ext_types = set()
for m in re.finditer(r'type:\s*"(\w+)"', ext):
    ext_types.add(m.group(1))
for m in re.finditer(r'case "(\w+)"', ext):
    ext_types.add(m.group(1))
print("扩展发送:", sorted(ext_types))