// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! Embeds the Windows resources — playdl.ico and a VERSIONINFO block — into
//! playdl-gui.exe. Explorer, the taskbar, and Task Manager's "Startup apps"
//! page all read these, so without them the exe gets a generic icon and the
//! login item shows an anonymous name instead of "PlayDL Download Manager".
//!
//! The .rc is generated here (version comes from Cargo.toml) and compiled by
//! embed-resource: rc.exe on a Windows host, llvm-rc when cross-compiling
//! with cargo-xwin (scripts/build-windows-installer.sh already puts Homebrew
//! LLVM on PATH).

use std::path::PathBuf;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico = manifest_dir
        .join("../../docs/playdl.ico")
        .canonicalize()
        .expect("docs/playdl.ico not found");
    println!("cargo:rerun-if-changed={}", ico.display());

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut nums = version.split('.').map(|p| p.parse::<u16>().unwrap_or(0));
    let (maj, min, pat) = (
        nums.next().unwrap_or(0),
        nums.next().unwrap_or(0),
        nums.next().unwrap_or(0),
    );

    // rc string literals treat backslash as an escape; a Windows-host path
    // needs them doubled (the macOS/Linux cross-build path has none).
    let ico_rc = ico.to_string_lossy().replace('\\', "\\\\");
    let rc = format!(
        r#"1 ICON "{ico_rc}"
1 VERSIONINFO
FILEVERSION {maj},{min},{pat},0
PRODUCTVERSION {maj},{min},{pat},0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "ProductName", "PlayDL Download Manager"
      VALUE "FileDescription", "PlayDL Download Manager"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "CompanyName", "leeymxz"
      VALUE "LegalCopyright", "(C) 2026 leeymxz. GPL-3.0-or-later."
      VALUE "OriginalFilename", "playdl-gui.exe"
      VALUE "InternalName", "playdl-gui"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );

    let rc_path = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("playdl-gui.rc");
    std::fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
