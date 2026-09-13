import sys

sys.stdout.reconfigure(encoding="utf-8")
text = open(
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\extensions\chrome\background.js",
    encoding="utf-8",
).read()

# 找 native() 函数
idx = text.find("function native")
if idx < 0:
    idx = text.find("async function native")
print("=== native() ===")
print(text[idx : idx + 700])
print()

# 找 HOST 常量使用
for m in __import__("re").finditer(r"native\(\)|await native|viaHost", text):
    pass
idx2 = text.find("HOST")
print("=== HOST 相关 ===")
for m in __import__("re").finditer(r".{0,50}HOST.{0,70}", text):
    print(f"  ...{m.group(0).replace(chr(10),' ')}...")
    print()