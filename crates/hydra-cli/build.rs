// PlayDL Windows resource — embeds playdl.ico and VERSIONINFO into playdl.exe / pdl.exe
// so Explorer shows the PlayDL icon instead of a generic one.

use std::path::PathBuf;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let workspace_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../..")
        .canonicalize()
        .expect("workspace root not found");

    let ico = workspace_dir.join("docs/playdl.ico");
    if !ico.exists() {
        // Fallback: try the root docs folder
        let ico2 = workspace_dir.join("docs/playdl.ico");
        if !ico2.exists() {
            panic!("playdl.ico not found at {} or {}", ico.display(), ico2.display());
        }
    }
    println!("cargo:rerun-if-changed={}", ico.display());

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut nums = version.split('.').map(|p| p.parse::<u16>().unwrap_or(0));
    let (maj, min, pat) = (
        nums.next().unwrap_or(0),
        nums.next().unwrap_or(0),
        nums.next().unwrap_or(0),
    );

    let ico_rc = ico.to_string_lossy().replace('\\', "\\\\");
    let product_name = "PlayDL Download Manager";
    let company = "leeymxz";
    let copyright = "(C) 2026 leeymxz. GPL-3.0-or-later.";

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
      VALUE "ProductName", "{product_name}"
      VALUE "FileDescription", "PlayDL Multi-Source Download Accelerator"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "CompanyName", "{company}"
      VALUE "LegalCopyright", "{copyright}"
      VALUE "OriginalFilename", "playdl.exe"
      VALUE "InternalName", "playdl"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );

    let rc_path = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("playdl.rc");
    std::fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_required()
        .unwrap();
}