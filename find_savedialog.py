import os
import sys

sys.stdout.reconfigure(encoding="utf-8")

base = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\crates\hydra-gui\src"
keywords = ["save_file", "saveFile", "SaveAs", "save_as", "FileDialog", "save_dialog", "set_directory", "set_file_name", "rfd::", "FileDialog::"]

for root, dirs, files in os.walk(base):
    for f in files:
        if not f.endswith(".rs"):
            continue
        p = os.path.join(root, f)
        lines = open(p, encoding="utf-8").read().splitlines()
        for i, line in enumerate(lines, 1):
            for kw in keywords:
                if kw.lower() in line.lower():
                    print(f"{os.path.relpath(p, base)}:{i}: {line.strip()[:110]}")
                    break
print("DONE")