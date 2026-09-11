from PIL import Image
import os

# 从透明 Logo 生成扩展图标
src = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\docs\logo.png"
out_dir = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\dist\playdl-extension\chrome\icons"
os.makedirs(out_dir, exist_ok=True)

img = Image.open(src).convert("RGBA")
# 图标一般是方形的，裁剪到中心正方形区域
w, h = img.size
side = min(w, h)
left = (w - side) // 2
top = (h - side) // 2
img = img.crop((left, top, left + side, top + side))

for size in [16, 32, 48, 128]:
    resized = img.resize((size, size), Image.LANCZOS)
    resized.save(os.path.join(out_dir, f"icon{size}.png"))
    print(f"icon{size}.png saved")

print("DONE")