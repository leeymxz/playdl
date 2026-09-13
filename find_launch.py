import sys

sys.stdout.reconfigure(encoding="utf-8")
text = open(
    r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\extensions\chrome\background.js",
    encoding="utf-8",
).read()

for kw in ["launchApp", "launch_app", "spawn", "nativeMessaging", "playdl-gui", "connectNative", "hostExists", "tryLaunch", "wakeApp", "bootApp"]:
    idx = text.find(kw)
    if idx >= 0:
        line_no = text[:idx].count("\n") + 1
        snippet = text[max(0, idx - 60) : idx + 160].replace("\n", " ")
        print(f"[{kw}] @ L{line_no}")
        print(f"  ...{snippet}...")
        print()
print("DONE")