#!/usr/bin/env bash
# Copyright (C) 2026 leeymxz
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Install the latest PlayDL release on Linux or macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/leeymxz/playdl/main/install.sh | bash
#   ... | bash -s -- --cli            # CLI only (default installs the GUI bundle)
#   ... | bash -s -- --version vx.x.x # pin a release instead of latest
#   ... | bash -s -- --beta           # newest -rc pre-release when ahead of latest
#   ... | bash -s -- --prefix ~/.local
#   ... | bash -s -- --app-dir ~/Applications # macOS app dir
#
# Default install is the GUI bundle.
#
# Both also get the browser extensions + native-host installer in
# <prefix>/share/playdl. On Linux this uses the release tarball. --cli
# installs only the playdl binary.
#
# Every mode also links <prefix>/bin/pdl to the CLI.

set -euo pipefail

REPO="leeymxz/playdl"
MODE="gui"
VERSION=""
PREFIX=""
APP_DIR=""
BETA=0

APP_NAME="PlayDL Download Manager"
BUNDLE_ID="io.github.leeymxz.playdl"

usage() {
  cat >&2 <<EOF
Usage: install.sh [--cli] [--version vX.Y.Z] [--beta] [--prefix DIR]

  --cli            install only the playdl CLI binary
  --version TAG    install a specific release tag (default: latest)
  --beta           install the newest -rc pre-release when it is ahead of the
                   latest stable release (otherwise the stable release)
  --prefix DIR     install root (default: /usr/local, falling back to ~/.local)
  --app-dir DIR    macOS only: where "$APP_NAME.app" is installed
                   (default: /Applications, falling back to ~/Applications)
EOF
  exit 2
}

while [ $# -gt 0 ]; do
  case "$1" in
    --cli) MODE="cli" ;;
    --gui) MODE="gui" ;;
    --version) VERSION="$2"; shift ;;
    --beta) BETA=1 ;;
    --prefix) PREFIX="$2"; shift ;;
    --app-dir) APP_DIR="$2"; shift ;;
    -h|--help) usage ;;
    *) echo "unknown option: $1" >&2; usage ;;
  esac
  shift
done

case "$(uname -s)" in
  Linux)  OS="linux" ;;
  Darwin) OS="macos" ;;
  *) echo "error: unsupported OS $(uname -s) (use install.ps1 on Windows)" >&2; exit 1 ;;
esac

case "$(uname -m)" in
  x86_64|amd64)  ARCH="amd64" ;;
  aarch64|arm64) ARCH="arm64" ;;
  *) echo "error: unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac

fetch() {
  if command -v curl >/dev/null 2>&1; then
    if [ $# -eq 2 ]; then curl -fSL --proto '=https' -o "$2" "$1"; else curl -fsSL --proto '=https' "$1"; fi
  elif command -v wget >/dev/null 2>&1; then
    if [ $# -eq 2 ]; then wget -qO "$2" "$1"; else wget -qO- "$1"; fi
  else
    echo "error: need curl or wget" >&2; exit 1
  fi
}

ver_gt() {
  awk -v a="${1#v}" -v b="${2#v}" 'BEGIN{
    sub(/[-+].*/, "", a); sub(/[-+].*/, "", b)
    n = split(a, x, "."); m = split(b, y, ".")
    for (i = 1; i <= 3; i++) {
      ai = (i <= n ? x[i] : 0) + 0; bi = (i <= m ? y[i] : 0) + 0
      if (ai > bi) exit 0
      if (ai < bi) exit 1
    }
    exit 1
  }'
}

if [ -n "${SUDO_USER:-}" ] && [ "${SUDO_USER}" != root ] && [ "$(id -u)" = 0 ]; then
  AS_USER="$SUDO_USER"
  USER_HOME=$(eval echo "~$SUDO_USER")
else
  AS_USER=""
  USER_HOME="$HOME"
fi

own_user() {
  [ -n "$AS_USER" ] || return 0
  chown -R "$AS_USER" "$@" 2>/dev/null || true
}

run_as_user() {
  if [ -n "$AS_USER" ]; then
    sudo -u "$AS_USER" -H "$@"
  else
    "$@"
  fi
}

build_macos_app() {
  local src="$1" app="$2" staged="$TMP/bundle/$APP_NAME.app"
  mkdir -p "$staged/Contents/MacOS" "$staged/Contents/Resources"

  install -m 755 "$src/playdl-gui" "$staged/Contents/MacOS/$APP_NAME"
  install -m 755 "$src/playdl" "$staged/Contents/MacOS/playdl"
  install -m 755 "$src/playdl-host" "$staged/Contents/MacOS/playdl-host"
  if [ -f "$src/playdl-updater" ]; then
    install -m 755 "$src/playdl-updater" "$staged/Contents/MacOS/playdl-updater"
  fi

  if [ -f "$src/logo.png" ] && command -v iconutil >/dev/null 2>&1; then
    local iconset="$TMP/playdl.iconset" size
    mkdir -p "$iconset"
    for size in 16 32 64 128 256 512; do
      sips -z "$size" "$size" "$src/logo.png" \
        --out "$iconset/icon_${size}x${size}.png" >/dev/null 2>&1 || true
      sips -z "$((size * 2))" "$((size * 2))" "$src/logo.png" \
        --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null 2>&1 || true
    done
    iconutil -c icns "$iconset" -o "$staged/Contents/Resources/playdl.icns" 2>/dev/null ||
      echo "warning: could not build the app icon from logo.png" >&2
  fi

  if [ -d "$src/man" ]; then
    mkdir -p "$staged/Contents/Resources/man/man1"
    install -m 644 "$src"/man/*.1 "$staged/Contents/Resources/man/man1/"
  fi

  cat > "$staged/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key><string>${APP_NAME}</string>
    <key>CFBundleExecutable</key><string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
    <key>CFBundleVersion</key><string>${VER%%-*}</string>
    <key>CFBundleShortVersionString</key><string>${VER%%-*}</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleIconFile</key><string>playdl</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
</dict>
</plist>
PLIST

  codesign --force --deep -s - "$staged" >/dev/null 2>&1 ||
    echo "warning: could not sign the app; macOS may deny it Downloads access" >&2

  if pgrep -f "$app/Contents/MacOS/" >/dev/null 2>&1; then
    echo "quitting the running $APP_NAME..."
    run_as_user osascript -e "quit app \"$APP_NAME\"" >/dev/null 2>&1 || true
    sleep 2
    pkill -f "$app/Contents/MacOS/" 2>/dev/null || true
  fi

  $APP_SUDO mkdir -p "$APP_DIR"
  $APP_SUDO rm -rf "$app"
  $APP_SUDO ditto "$staged" "$app"
  if [ -n "$APP_SUDO" ]; then
    $APP_SUDO chown -R "${AS_USER:-$(id -un)}" "$app" 2>/dev/null || true
  fi

  local lsregister="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
  if [ -x "$lsregister" ]; then
    "$lsregister" -f "$app" >/dev/null 2>&1 || true
  fi
  echo "installed $app"
}

link_bin() {
  $SUDO ln -sfn "$2" "$BIN_DIR/$1"
  echo "linked $BIN_DIR/$1 -> $2"
}

link_cli_alias() {
  local alias_path="$BIN_DIR/pdl"
  if [ -L "$alias_path" ]; then
    case "$(readlink "$alias_path")" in
      playdl|"$BIN_DIR/playdl") ;;
      *) echo "note: $alias_path points somewhere else; left alone" >&2; return 0 ;;
    esac
  elif [ -e "$alias_path" ]; then
    echo "note: $alias_path already exists; left alone (use playdl instead)" >&2
    return 0
  fi
  $SUDO ln -sfn playdl "$alias_path"
  echo "linked $alias_path -> playdl"
}

if [ -z "$VERSION" ]; then
  RELEASE_JSON=$(fetch "https://api.github.com/repos/${REPO}/releases/latest") || RELEASE_JSON=""
  VERSION=$(printf '%s\n' "$RELEASE_JSON" | grep -m1 '"tag_name"' | cut -d'"' -f4 || true)
  [ -n "$VERSION" ] || { echo "error: could not resolve the latest release tag" >&2; exit 1; }
  if [ "$BETA" = 1 ]; then
    LIST_JSON=$(fetch "https://api.github.com/repos/${REPO}/releases?per_page=30") || LIST_JSON=""
    RC_TAG=$(printf '%s\n' "$LIST_JSON" | grep -o '"tag_name": *"[^"]*"' | cut -d'"' -f4 | grep -m1 -- '-rc' || true)
    if [ -n "$RC_TAG" ] && ver_gt "$RC_TAG" "$VERSION"; then
      VERSION="$RC_TAG"
    fi
  fi
fi
VER="${VERSION#v}"
CORE="${VER%%-*}"
if [ "$MODE" = cli ]; then
  ASSET_PREFIX="playdl-cli"
else
  ASSET_PREFIX="playdl"
fi
if [ "$CORE" != "$VER" ]; then
  CANDIDATES="$VER $CORE"
else
  CANDIDATES="$VER"
fi
DL_BASE="https://github.com/${REPO}/releases/download/${VERSION}"

SUDO=""
if [ -z "$PREFIX" ]; then
  if [ -w /usr/local/bin ] 2>/dev/null || [ -w /usr/local ]; then
    PREFIX="/usr/local"
  elif command -v sudo >/dev/null 2>&1; then
    PREFIX="/usr/local"; SUDO="sudo"
  else
    PREFIX="$HOME/.local"
  fi
elif [ ! -w "$PREFIX" ] && [ -e "$PREFIX" ] && command -v sudo >/dev/null 2>&1; then
  SUDO="sudo"
fi
BIN_DIR="$PREFIX/bin"
SHARE_DIR="$PREFIX/share/playdl"

APP=""
APP_SUDO=""
if [ "$OS" = macos ] && [ "$MODE" = gui ]; then
  if [ -z "$APP_DIR" ]; then
    if [ -w /Applications ]; then
      APP_DIR="/Applications"
    elif command -v sudo >/dev/null 2>&1; then
      APP_DIR="/Applications"; APP_SUDO="sudo"
    else
      APP_DIR="$USER_HOME/Applications"
    fi
  elif [ -e "$APP_DIR" ] && [ ! -w "$APP_DIR" ] && command -v sudo >/dev/null 2>&1; then
    APP_SUDO="sudo"
  fi
  APP="$APP_DIR/$APP_NAME.app"
fi

echo "playdl ${VERSION} (${MODE}) -> ${PREFIX}  [${OS}/${ARCH}]"
if [ -n "$APP" ]; then echo "app bundle -> ${APP}"; fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

NAME=""
for CAND in $CANDIDATES; do
  TRY="${ASSET_PREFIX}-${CAND}-${OS}-${ARCH}"
  echo "downloading ${DL_BASE}/${TRY}.tar.gz"
  if [ "$CAND" = "$CORE" ]; then
    if fetch "${DL_BASE}/${TRY}.tar.gz" "$TMP/${TRY}.tar.gz"; then NAME="$TRY"; break; fi
  elif fetch "${DL_BASE}/${TRY}.tar.gz" "$TMP/${TRY}.tar.gz" 2>/dev/null; then
    NAME="$TRY"; break
  fi
  rm -f "$TMP/${TRY}.tar.gz"
done
[ -n "$NAME" ] || { echo "error: no ${MODE} archive for ${OS}/${ARCH} in release ${VERSION}" >&2; exit 1; }
tar -xzf "$TMP/${NAME}.tar.gz" -C "$TMP"
SRC="$TMP/$NAME"

$SUDO mkdir -p "$BIN_DIR"
if [ -n "$APP" ]; then
  :
else
  $SUDO install -m 755 "$SRC/playdl" "$BIN_DIR/playdl"
  echo "installed $BIN_DIR/playdl"
  link_cli_alias
fi

if [ -d "$SRC/man" ]; then
  MAN_DIR="$PREFIX/share/man/man1"
  $SUDO mkdir -p "$MAN_DIR"
  for m in "$SRC"/man/*.1; do
    $SUDO install -m 644 "$m" "$MAN_DIR/$(basename "$m")"
  done
  echo "installed man pages into $MAN_DIR (try: man playdl)"
fi

if [ "$MODE" = gui ]; then
  HOST_BIN="$BIN_DIR/playdl-host"
  if [ -n "$APP" ]; then
    build_macos_app "$SRC" "$APP"
    HOST_BIN="$APP/Contents/MacOS/playdl-host"
    link_bin playdl "$APP/Contents/MacOS/playdl"
    link_cli_alias
    link_bin playdl-gui "$APP/Contents/MacOS/$APP_NAME"
    link_bin playdl-host "$APP/Contents/MacOS/playdl-host"
  else
    $SUDO install -m 755 "$SRC/playdl-gui" "$BIN_DIR/playdl-gui"
    $SUDO install -m 755 "$SRC/playdl-host" "$BIN_DIR/playdl-host"
    echo "installed $BIN_DIR/playdl-gui"
    echo "installed $BIN_DIR/playdl-host"
    if [ -f "$SRC/playdl-updater" ]; then
      $SUDO install -m 755 "$SRC/playdl-updater" "$BIN_DIR/playdl-updater"
      echo "installed $BIN_DIR/playdl-updater"
    fi
  fi

  $SUDO rm -rf "$SHARE_DIR"
  $SUDO mkdir -p "$SHARE_DIR"
  $SUDO cp -R "$SRC/extensions" "$SRC/scripts" "$SHARE_DIR/"
  echo "installed $SHARE_DIR (browser extensions + native-host installer)"

  if [ "$OS" = linux ]; then
    DATA_DIRS=("$USER_HOME/.local/share")
    case "$PREFIX" in
      "$USER_HOME"*) ;;
      *) if [ -d "$PREFIX" ] && { [ -w "$PREFIX" ] || [ -n "$SUDO" ]; }; then
           DATA_DIRS+=("$PREFIX/share")
         fi ;;
    esac

    write_data() {
      local mode="$1" src="$2" dest="$3" sudo=""
      case "$dest" in "$USER_HOME"*) ;; *) sudo="$SUDO" ;; esac
      $sudo mkdir -p "$(dirname "$dest")"
      $sudo install -m "$mode" "$src" "$dest"
      case "$dest" in "$USER_HOME"*) own_user "$(dirname "$dest")" ;; esac
      echo "installed $dest"
    }

    scale_icon() {
      if command -v magick >/dev/null 2>&1; then
        magick "$SRC/logo.png" -resize "${1}x${1}" "$2" 2>/dev/null
      elif command -v convert >/dev/null 2>&1; then
        convert "$SRC/logo.png" -resize "${1}x${1}" "$2" 2>/dev/null
      elif python3 -c 'import PIL' 2>/dev/null; then
        python3 -c 'import sys
from PIL import Image
Image.open(sys.argv[1]).convert("RGBA").resize(
    (int(sys.argv[2]),) * 2, Image.LANCZOS).save(sys.argv[3])' \
          "$SRC/logo.png" "$1" "$2" 2>/dev/null
      else
        return 1
      fi
    }

    if [ -f "$SRC/logo.png" ]; then
      for data in "${DATA_DIRS[@]}"; do
        write_data 644 "$SRC/logo.png" "$data/icons/hicolor/256x256/apps/playdl.png"
        for size in 16 24 32 48 64 128; do
          if scale_icon "$size" "$TMP/icon-${size}.png"; then
            write_data 644 "$TMP/icon-${size}.png" \
              "$data/icons/hicolor/${size}x${size}/apps/playdl.png"
          fi
        done
      done
    fi

    cat > "$TMP/playdl.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=PlayDL Download Manager
GenericName=Download Manager
Comment=Multi-connection download accelerator
Exec=$BIN_DIR/playdl-gui
TryExec=$BIN_DIR/playdl-gui
Icon=playdl
Terminal=false
Categories=Network;FileTransfer;
Keywords=download;downloader;accelerator;http;
StartupNotify=true
StartupWMClass=playdl
EOF
    for data in "${DATA_DIRS[@]}"; do
      write_data 644 "$TMP/playdl.desktop" "$data/applications/playdl.desktop"
      command -v update-desktop-database >/dev/null 2>&1 &&
        update-desktop-database -q "$data/applications" 2>/dev/null || true
      command -v gtk-update-icon-cache >/dev/null 2>&1 &&
        gtk-update-icon-cache -qt "$data/icons/hicolor" 2>/dev/null || true
    done
  fi

  if command -v python3 >/dev/null 2>&1; then
    run_as_user bash "$SHARE_DIR/scripts/install-native-host.sh" --no-build --host-bin "$HOST_BIN" \
      || echo "warning: native-messaging host registration failed; rerun: $SHARE_DIR/scripts/install-native-host.sh --no-build --host-bin $HOST_BIN" >&2
  else
    echo "note: python3 not found; to enable browser integration run:" >&2
    echo "  $SHARE_DIR/scripts/install-native-host.sh --no-build --host-bin $HOST_BIN" >&2
  fi

  EXT_DIR="$SHARE_DIR/extensions"
  XPI=""
  for f in "$EXT_DIR"/playdl-firefox-*.xpi; do
    if [ -f "$f" ]; then XPI="$f"; break; fi
  done
  [ -n "$XPI" ] || XPI="$EXT_DIR/firefox/manifest.json"
  cat <<EOF

Browser extension -- the builds in $EXT_DIR are unsigned,
so each browser loads them through its developer mode:

  Chrome / Edge / Brave / Opera / Vivaldi / Arc / Chromium
    1. open chrome://extensions (edge://extensions, brave://extensions, ...)
    2. turn on "Developer mode" (top right in Chrome; bottom left in Edge)
    3. click "Load unpacked" and select:
           $EXT_DIR/chrome

  Firefox -- temporary (every edition; removed at the next restart)
    1. open about:debugging#/runtime/this-firefox
    2. click "Load Temporary Add-on..." and select:
           $XPI

  Firefox -- permanent (Developer Edition, Nightly and ESR only)
    1. in about:config set xpinstall.signatures.required = false
    2. in about:addons use the gear icon > "Install Add-on From File..."
       and pick the .xpi in $EXT_DIR

Restart the browser once so it picks up PlayDL's native-messaging manifest,
then check the PlayDL toolbar icon: the status dot turns green when the
extension has reached the app.
EOF
  if [ -f "$EXT_DIR/INSTALL.txt" ]; then
    echo "Full instructions: $EXT_DIR/INSTALL.txt"
  fi
fi

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "note: $BIN_DIR is not on your PATH" >&2 ;;
esac

echo "done."