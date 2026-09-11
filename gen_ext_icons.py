from PIL import Image
import os

# 从你的新 Logo 生成扩展图标（确保透明背景）
src = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\docs\logo.png"
out_dirs = [
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\extensions\chrome\icons",
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\extensions\firefox\icons",
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\dist\playdl-extension\chrome\icons",
]
for out_dir in out_dirs:
    os.makedirs(out_dir, exist_ok=True)

img = Image.open(src).convert("RGBA")
print(f"原图: {img.size}, 模式: {img.mode}")

# 裁剪到中心正方形
w, h = img.size
side = min(w, h)
left = (w - side) // 2
top = (h - side) // 2
img = img.crop((left, top, left + side, top + side))
print(f"裁剪后: {img.size}")

for out_dir in out_dirs:
    for size in [16, 32, 48, 128]:
        resized = img.resize((size, size), Image.LANCZOS)
        resized.save(os.path.join(out_dir, f"icon{size}.png"))
    print(f"OK: {out_dir}")

print("DONE - all icons regenerated from new logo")