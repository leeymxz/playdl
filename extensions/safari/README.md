# PlayDL Safari Extension

Same code, same transport, same experience as the
[Chrome extension](../chrome/README.md) — with one platform limit that
cannot be worked around (see below).

## Why this needs no separate source tree

The extension code in `extensions/chrome` is browser-neutral: it binds
whichever namespace exists (`browser` on Safari, `chrome` on Chromium),
handles both native-messaging dialects (Safari returns a promise, Chrome
takes a callback), and feature-detects every API it uses. Only the manifest
differs, so `scripts/sync-extension-resources.sh safari` copies the shared files next
to Safari's own manifest into `Resources/`.

**Edit `extensions/chrome/*`, never `extensions/safari/Resources/*`** — the
latter is regenerated on every sync.

The transport is the reason this works so cleanly: since the extension talks
to PlayDL over a WebSocket on a fixed loopback port (6799), Safari needs no
special IPC at all. The app accepts `safari-web-extension://…` origins
exactly as it accepts `chrome-extension://…`.

## What works, and the one thing that does not

| Feature | Chrome | Safari |
|---|---|---|
| Right-click → Download with PlayDL | ✅ | ✅ |
| Selection pill over highlighted links | ✅ | ✅ |
| Download all links | ✅ | ✅ |
| Popup, capture toggle, status badge | ✅ | ✅ |
| Media sniffing | ✅ | ⚠️ only where Safari exposes `webRequest` |
| **Automatic download capture** | ✅ | ❌ **impossible** |

Safari has no `downloads` API — Apple does not expose download events to
extensions at all, so there is nothing to intercept. Every other browser
integration on macOS has the same limit; it is not a gap in this code. Use
the right-click menu or the selection pill instead.

## Build & install

Requires **full Xcode** (Command Line Tools alone cannot build app
extensions):

```bash
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
```

Then:

```bash
scripts/build-safari-extension.sh
```

The script syncs the resources, generates the wrapper app, installs
[SafariWebExtensionHandler.swift](SafariWebExtensionHandler.swift), disables
the app sandbox (the handler must read `~/.config/playdl/ipc.json` and launch
the app), allows cleartext loopback via ATS, and builds.

Afterwards: open the built app once, then Safari → Settings → Extensions →
enable **Hydra**. For an unsigned dev build also tick Develop → Allow
Unsigned Extensions (Safari clears this on restart).

## Native handler

[SafariWebExtensionHandler.swift](SafariWebExtensionHandler.swift) is the
fallback path, mirroring `playdl-host`: it forwards a request to the app's
line-protocol socket using the token from `ipc.json`, and launches
"PlayDL Download Manager" when nothing answers. Day to day the WebSocket
carries the traffic; the handler exists so the first click can start the app.
