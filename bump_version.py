import os
import sys

sys.stdout.reconfigure(encoding="utf-8")

root = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader"
skip_dirs = {"target", ".git", "dist", "generated-images", "node_modules"}
skip_exts = {".exe", ".zip", ".ico", ".png", ".ttf", ".db", ".lock"}

exts = {".toml", ".iss", ".md", ".ps1", ".bat", ".yml", ".yaml", ".json", ".rs", ".html", ".js"}

count = 0
for dirpath, dirs, files in os.walk(root):
    dirs[:] = [d for d in dirs if d not in skip_dirs]
    for f in files:
        ext = os.path.splitext(f)[1].lower()
        if ext not in exts:
            continue
        p = os.path.join(dirpath, f)
        try:
            text = open(p, encoding="utf-8").read()
        except Exception:
            continue
        if "0.1.0" in text:
            new = text.replace("0.1.0", "0.2.0")
            open(p, "w", encoding="utf-8").write(new)
            print(f"Updated: {os.path.relpath(p, root)}")
            count += 1

print(f"DONE: {count} files updated to 0.2.0")