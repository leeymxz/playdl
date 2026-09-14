import os
import sys
import zipfile

sys.stdout.reconfigure(encoding="utf-8")
base = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader"
ver = "0.3.5"

# 源目录
win_dir = os.path.join(base, "build", f"playdl-{ver}-windows-amd64")
out_zip = os.path.join(base, "dist", f"playdl-{ver}-windows-amd64.zip")

# 用 zipfile 打包（不会像 Compress-Archive 那样被文件锁卡住）
def zipdir(src, zf):
    for root, dirs, files in os.walk(src):
        for f in files:
            fp = os.path.join(root, f)
            arc = os.path.relpath(fp, src)
            try:
                zf.write(fp, arc)
            except PermissionError:
                print(f"SKIP (locked): {arc}")

if not os.path.isdir(win_dir):
    print(f"ERROR: {win_dir} not exists")
    sys.exit(1)

if os.path.exists(out_zip):
    os.remove(out_zip)

with zipfile.ZipFile(out_zip, "w", zipfile.ZIP_DEFLATED) as zf:
    zipdir(win_dir, zf)

size = os.path.getsize(out_zip) / (1024 * 1024)
print(f"OK: {os.path.basename(out_zip)} ({size:.1f} MB)")

# 扩展
for name, src in [
    ("playdl-extension-chrome.zip", os.path.join(base, "extensions", "chrome")),
    ("playdl-extension-firefox.zip", os.path.join(base, "extensions", "firefox")),
]:
    out = os.path.join(base, "dist", name)
    if os.path.exists(out):
        os.remove(out)
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as zf:
        zipdir(src, zf)
    sz = os.path.getsize(out) / 1024
    print(f"OK: {name} ({sz:.0f} KB)")

print("DONE")