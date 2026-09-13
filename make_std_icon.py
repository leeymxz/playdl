from PIL import Image
import os

# 用 3D Logo 生成标准图标
src = r"C:\Users\Administrator\Documents\Loomy Workspace\.loomy-attachments\ses_f76845ca9ffeBQ1zTXnzBFKRTt\1789125963074-0-playdl-logo.png"
dir_ = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader"

img = Image.open(src)
print(f"原图: {img.size}, 模式: {img.mode}")

# 转 RGBA（保留透明）
if img.mode != "RGBA":
    img = img.convert("RGBA")

# 裁剪到中心正方形
w, h = img.size
side = min(w, h)
left = (w - side) // 2
top = (h - side) // 2
square = img.crop((left, top, left + side, top + side))
print(f"裁剪后: {square.size}")

# 保存为 docs/logo.png（透明背景）
square.save(os.path.join(dir_, "docs", "logo.png"), "PNG")
print("OK: docs/logo.png")

# 生成多尺寸 ICO（含 256/64/48/32/24/16）
sizes = [(256, 256), (64, 64), (48, 48), (32, 32), (24, 24), (16, 16)]
frames = [square.resize(s, Image.LANCZOS) for s in sizes]
frames[0].save(
    os.path.join(dir_, "docs", "playdl.ico"),
    format="ICO",
    sizes=sizes,
    append_images=frames[1:],
)
print("OK: docs/playdl.ico")

# 扩展图标
for ext_dir in [
    os.path.join(dir_, "extensions", "chrome", "icons"),
    os.path.join(dir_, "extensions", "firefox", "icons"),
]:
    os.makedirs(ext_dir, exist_ok=True)
    for size in [16, 32, 48, 128]:
        square.resize((size, size), Image.LANCZOS).save(os.path.join(ext_dir, f"icon{size}.png"))
    print(f"OK: {ext_dir}")

print("DONE")