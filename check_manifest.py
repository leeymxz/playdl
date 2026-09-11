import json, sys, os
sys.stdout.reconfigure(encoding='utf-8')
base = r"C:\Users\Administrator\Documents\Loomy Workspace\my-downloader\extensions\chrome"
d = json.load(open(os.path.join(base, "manifest.json"), encoding="utf-8"))
print("manifest.json valid OK")
print("name:", d.get("name"))
print("version:", d.get("version"))
print("key present:", "key" in d)

refs = []
for cs in d.get("content_scripts", []):
    refs += cs.get("js", [])
bg = d.get("background", {})
sw = bg.get("service_worker", "")
if sw:
    refs.append(sw)
if "default_popup" in d.get("action", {}):
    refs.append(d["action"]["default_popup"])
for icon in d.get("icons", {}).values():
    refs.append(icon)
for r in set(refs):
    p = os.path.join(base, r)
    status = "OK" if os.path.exists(p) else "MISSING!"
    print(f"  {r}: {status}")
print("DONE")