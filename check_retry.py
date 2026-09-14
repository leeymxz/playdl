import sys

sys.stdout.reconfigure(encoding="utf-8")
text = open(
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\crates\hydra-gui\src\update.rs",
    encoding="utf-8",
).read()

idx = text.find("download_to_file")
if idx > 0:
    print(f"download_to_file @ L{text[:idx].count(chr(10))+1}")

for kw in ["retry", "Retry", "resume", "append", "Range", "download_to_file"]:
    idx2 = text.find(kw)
    if idx2 > 0:
        snippet = text[max(0, idx2 - 50) : idx2 + 80].replace("\n", " ")
        print(f"[{kw}] L{text[:idx2].count(chr(10))+1}: ...{snippet}...")