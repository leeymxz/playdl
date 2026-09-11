// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! Known file-extension descriptions for hover hints on links: hovering a
//! URL that ends in a recognised extension shows `DMG — Apple Disk Image
//! (macOS)` so the user knows what kind of file the link delivers.

use crate::i18n::tr;

/// English description for a known lower-case extension.
///
/// The table sticks to formats that actually travel through a download
/// manager; source-code and office-internal formats nobody downloads
/// stand-alone are left out on purpose so unknown really means unknown.
fn describe(ext: &str) -> Option<&'static str> {
    Some(match ext {
        // Archives
        "zip" => "ZIP archive",
        "rar" => "RAR archive",
        "7z" => "7-Zip archive",
        "tar" => "tar archive",
        "gz" => "gzip-compressed file",
        "bz2" => "bzip2-compressed file",
        "xz" => "XZ-compressed file",
        "zst" => "Zstandard-compressed file",
        "tar.gz" | "tgz" => "gzip-compressed tar archive",
        "tar.bz2" | "tbz2" => "bzip2-compressed tar archive",
        "tar.xz" | "txz" => "XZ-compressed tar archive",
        "tar.zst" => "Zstandard-compressed tar archive",
        // Installers / programs
        "dmg" => "Apple Disk Image (macOS)",
        "pkg" => "Installer package (macOS)",
        "exe" => "Executable program (Windows)",
        "msi" => "Windows Installer package",
        "msix" => "Windows app package",
        "apk" => "Android app package",
        "ipa" => "iOS app package",
        "deb" => "Debian/Ubuntu software package",
        "rpm" => "RPM software package (Fedora/RHEL)",
        "appimage" => "Portable Linux application",
        "flatpak" => "Flatpak application bundle (Linux)",
        "snap" => "Snap application package (Linux)",
        "jar" => "Java application archive",
        "iso" => "Disc image",
        "img" => "Raw disk image",
        "bin" => "Binary file",
        // Documents
        "pdf" => "PDF document",
        "epub" => "EPUB e-book",
        "mobi" => "Kindle e-book",
        "doc" | "docx" => "Word document",
        "xls" | "xlsx" => "Excel spreadsheet",
        "ppt" | "pptx" => "PowerPoint presentation",
        "odt" => "OpenDocument text",
        "ods" => "OpenDocument spreadsheet",
        "odp" => "OpenDocument presentation",
        "rtf" => "Rich Text document",
        "txt" => "Plain text file",
        "csv" => "Comma-separated values",
        "json" => "JSON data",
        "xml" => "XML data",
        // Audio
        "mp3" => "MP3 audio",
        "aac" | "m4a" => "AAC audio",
        "flac" => "FLAC lossless audio",
        "ogg" | "oga" => "Ogg audio",
        "opus" => "Opus audio",
        "wav" => "WAV audio",
        "wma" => "Windows Media audio",
        // Video
        "mp4" | "m4v" => "MPEG-4 video",
        "mkv" => "Matroska video",
        "webm" => "WebM video",
        "avi" => "AVI video",
        "mov" => "QuickTime video",
        "wmv" => "Windows Media video",
        "flv" => "Flash video",
        "mpg" | "mpeg" => "MPEG video",
        "ts" => "MPEG transport stream",
        "3gp" => "3GPP mobile video",
        // Images
        "jpg" | "jpeg" => "JPEG image",
        "png" => "PNG image",
        "gif" => "GIF image",
        "webp" => "WebP image",
        "svg" => "SVG vector image",
        "bmp" => "Bitmap image",
        "tif" | "tiff" => "TIFF image",
        "ico" => "Icon image",
        "heic" => "HEIC image",
        "psd" => "Photoshop document",
        // Fonts / misc downloads
        "ttf" => "TrueType font",
        "otf" => "OpenType font",
        "woff" | "woff2" => "Web font",
        "torrent" => "BitTorrent metadata file",
        // Web pages, stream manifests, mirror lists — what the batch
        // dialog's File Type column names, and what "Hide HTML files" hides.
        "html" | "htm" | "xhtml" | "shtml" => "HTML web page",
        "php" | "asp" | "aspx" | "jsp" => "Server web page",
        "m3u8" => "HLS stream manifest",
        "mpd" => "DASH stream manifest",
        "meta4" | "metalink" => "Metalink mirror list",
        _ => return None,
    })
}

/// Lower-case extension of the file a URL points at, compound spellings
/// (`tar.gz`) preferred over the bare last segment (`gz`). Query string and
/// fragment are ignored; `%xx` escapes are decoded so `Firefox%20153.0.4.dmg`
/// still ends in `dmg`.
fn url_ext(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let seg = path.rsplit('/').next().unwrap_or(path);
    let seg = percent_decode(seg).to_ascii_lowercase();
    let (stem, last) = seg.rsplit_once('.')?;
    if last.is_empty() || last.len() > 8 || !last.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    if let Some((_, mid)) = stem.rsplit_once('.') {
        let compound = format!("{mid}.{last}");
        if describe(&compound).is_some() {
            return Some(compound);
        }
    }
    Some(last.to_owned())
}

/// `%20` → space, etc. Invalid escapes pass through unchanged.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1), b.get(i + 2)) {
            (b'%', Some(&h), Some(&l)) if h.is_ascii_hexdigit() && l.is_ascii_hexdigit() => {
                let hex = [h, l];
                let v = u8::from_str_radix(std::str::from_utf8(&hex).unwrap(), 16).unwrap();
                out.push(v);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `DMG — Apple Disk Image (macOS)` when the URL's extension is known,
/// `None` otherwise (no tooltip beats a useless one).
pub fn hint_for_url(url: &str) -> Option<String> {
    let ext = url_ext(url)?;
    let desc = describe(&ext)?;
    Some(format!("{} — {}", ext.to_ascii_uppercase(), tr(desc)))
}

/// The batch dialog's File Type column: `ZIP archive` for a known
/// extension, `XYZ file` for an unknown one, plain `File` when the name has
/// no extension at all. Version-looking tails (`tool-v1.2.3`) count as no
/// extension rather than a `3 file`.
pub fn kind_for_name(name: &str) -> String {
    match url_ext(name).filter(|e| !e.chars().all(|c| c.is_ascii_digit())) {
        Some(ext) => match describe(&ext) {
            Some(d) => tr(d),
            None => format!("{} {}", ext.to_ascii_uppercase(), tr("file")),
        },
        None => tr("File"),
    }
}

/// Whether a name is a web page rather than a file worth downloading — the
/// batch dialog's "Hide HTML files" filter.
pub fn is_web_page(name: &str) -> bool {
    matches!(
        url_ext(name).as_deref(),
        Some("html" | "htm" | "xhtml" | "shtml" | "php" | "asp" | "aspx" | "jsp")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_encoded_urls() {
        assert_eq!(
            url_ext("https://cdn.mozilla.net/pub/firefox/Firefox%20153.0.4.dmg"),
            Some("dmg".into())
        );
        assert_eq!(
            url_ext("https://x.y/a/b/tool-v1.2.3.tar.gz?token=abc#frag"),
            Some("tar.gz".into())
        );
        assert_eq!(url_ext("https://x.y/archive.ZIP"), Some("zip".into()));
    }

    #[test]
    fn kinds_and_web_pages() {
        assert_eq!(kind_for_name("setup.tar.gz"), "gzip-compressed tar archive");
        assert_eq!(kind_for_name("weird.xyz"), "XYZ file");
        assert_eq!(kind_for_name("meilisearch-linux-aarch64"), "File");
        assert_eq!(kind_for_name("tool-v1.2.3"), "File");
        assert!(is_web_page("index.html"));
        assert!(is_web_page("download.php"));
        assert!(!is_web_page("archive.zip"));
        assert!(!is_web_page("readme"));
    }

    #[test]
    fn unknown_or_absent_extension() {
        assert_eq!(url_ext("https://x.y/meilisearch-linux-aarch64"), None);
        assert_eq!(hint_for_url("https://x.y/page.html5up"), None);
        assert_eq!(url_ext("https://x.y/"), None);
        // Version-looking tails parse as an extension but describe() rejects them.
        assert_eq!(hint_for_url("https://x.y/v1.2.3"), None);
    }

    #[test]
    fn hint_spelling() {
        assert_eq!(
            hint_for_url("https://x.y/Firefox%20153.0.4.dmg"),
            Some("DMG — Apple Disk Image (macOS)".into())
        );
        assert_eq!(hint_for_url("https://x.y/meilisearch-linux-aarch64"), None);
    }
}
