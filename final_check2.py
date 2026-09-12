import os
import sys

sys.stdout.reconfigure(encoding="utf-8")
root = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader"
skip = {
    "target", ".git", "dist", "generated-images", "node_modules",
    "hydra-gui", "hydra-cli", "hydra-core", "hydra-net", "hydra-ffi",
    "hydra-stream", "hydra-host", "hydra-updater",
}
found = []
for dp, dirs, files in os.walk(root):
    dirs[:] = [d for d in dirs if d not in skip]
    for f in files:
        ext = os.path.splitext(f)[1].lower()
        if ext not in {".rs", ".json", ".html", ".js", ".toml", ".md", ".ps1", ".bat", ".yml", ".iss", ".css"}:
            continue
        p = os.path.join(dp, f)
        # 跳过 CHANGELOG（历史记录保留）
        if f == "CHANGELOG.md":
            continue
        try:
            t = open(p, encoding="utf-8").read()
        except Exception:
            continue
        for i, line in enumerate(t.splitlines(), 1):
            low = line.lower()
            if "hydra" in low:
                s = line.strip()
                if s.startswith(("//", "#", "*", "/*")):
                    continue
                if "HYDRA_" in line:
                    continue
                if "crates/hydra" in low:
                    continue
                if 'path = "crates/hydra' in low:
                    continue
                found.append(f"{os.path.relpath(p, root)}:{i}: {s[:90]}")

print(f"User-visible (excl. CHANGELOG): {len(found)}")
for x in found[:50]:
    print(f"  {x}")
print("DONE")