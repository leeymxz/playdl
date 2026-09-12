import os
import sys

sys.stdout.reconfigure(encoding="utf-8")
root = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader"
skip = {"target", ".git", "dist", "generated-images", "node_modules"}
exts = {".toml", ".iss", ".md", ".ps1", ".bat", ".yml", ".rs", ".json", ".html", ".js"}
count = 0

for dp, dirs, files in os.walk(root):
    dirs[:] = [d for d in dirs if d not in skip]
    for f in files:
        if os.path.splitext(f)[1].lower() not in exts:
            continue
        p = os.path.join(dp, f)
        try:
            t = open(p, encoding="utf-8").read()
        except Exception:
            continue
        # 只替换 0.2.0 纯版本号（排除 release 下载链接/资产名里的 0.2.0）
        if "0.2.0" in t and "v0.2.0" not in t:
            new = t.replace("0.2.0", "0.3.0")
            open(p, "w", encoding="utf-8").write(new)
            print(f"Updated: {os.path.relpath(p, root)}")
            count += 1

print(f"DONE: {count} files")