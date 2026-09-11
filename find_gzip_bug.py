import re
import sys
import os

sys.stdout.reconfigure(encoding="utf-8")

base = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\extensions\chrome"
files = ["background.js", "content.js", "popup.js", "welcome.js"]

# 这个转换是有风险的：正则处理 ??= ?? ?. 需要很小心。
# 更稳妥的方式：确认遨游实际是哪个 Chromium 版本，再决定。
# 但用户报错明确是 'gzip' 读取 undefined，说明代码执行到某处对象是 undefined。
# 先搜 "DecompressionStream" / "CompressionStream" 之类标准 API —— 那个报错正是
# 浏览器不支持 DecompressionStream 时的典型错误。

for f in files:
    p = os.path.join(base, f)
    text = open(p, encoding="utf-8").read()
    if "DecompressionStream" in text:
        for i, line in enumerate(text.splitlines(), 1):
            if "DecompressionStream" in line or "CompressionStream" in line:
                print(f"{f}:{i}: {line.strip()[:100]}")
