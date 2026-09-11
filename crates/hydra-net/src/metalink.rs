// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Metalink: a mirror list, a size, and a set of digests, in one document.
//!
//! # Why this format is the one worth supporting
//!
//! Everything this scheduler does well needs three facts that HTTP alone cannot
//! supply: **who else has these bytes**, **how large the object is**, and **what
//! the bytes are supposed to hash to**. A plain URL gives none of them. The
//! engine has to discover the size with a probe, it has no way at all to learn
//! about a second source, and it can only verify the object if the user typed a
//! digest on the command line.
//!
//! A Metalink supplies all three up front, from a host that is usually *not* one
//! of the mirrors — which is what makes its digests worth more than a `.sha256`
//! served beside the artifact. That turns three properties on:
//!
//! * **Multi-source assembly with no validator handshake.** [`probe_all`] in the
//!   CLI keeps a mirror only if it agrees with the first on size AND on a strong
//!   validator, because without that agreement two mirrors could be serving
//!   different builds. A Metalink states the size and a content digest for the
//!   object itself, so agreement is established against the *document* rather
//!   than pairwise between mirrors — which is both stronger and cheaper.
//! * **Fault tolerance with a bench.** Nineteen mirrors and four connections
//!   means fifteen sources in reserve. A source that dies is replaced instead of
//!   stranding its range.
//! * **Localised repair.** `<pieces>` is a per-chunk digest list, which is
//!   exactly the manifest `manifest.rs` already knows how to verify against and
//!   repair from — so a corrupt chunk costs one chunk refetch from a different
//!   mirror rather than a whole re-download.
//!
//! # Two dialects, one model
//!
//! * **Metalink 3.0** (`http://www.metalinker.org/`) — what mirrormanager and
//!   most distribution redirectors still emit. Files live under `<files>`,
//!   digests under `<verification>`, and mirror preference is `preference`,
//!   **0-100, higher is better**.
//! * **Metalink 4.0 / RFC 5854** (`urn:ietf:params:xml:ns:metalink`, `.meta4`) —
//!   files are direct children of the root, digests are direct children of
//!   `<file>`, and mirror preference is `priority`, **1-999999, LOWER is
//!   better**.
//!
//! Those two preference scales run in opposite directions, which is a defect
//! waiting to happen: a reader that treats them as one number sends the most
//! work to the *worst* mirror in one of the two dialects, and nothing about the
//! resulting transfer looks wrong — it is merely slower than it should be, in a
//! way indistinguishable from an unlucky mirror. So the model here has exactly
//! one preference field, [`MetaUrl::priority`], always in the RFC 5854 direction
//! (lower is better), and the 3.0 reader converts. See
//! [`priority_from_preference`].
//!
//! # Metalink over HTTP (RFC 6249)
//!
//! The same information can arrive as response headers on an ordinary download:
//! `Link: <url>; rel=duplicate; pri=1; geo=de` names a mirror, and
//! `Link: <url>; rel=describedby; type="application/metalink4+xml"` points at a
//! full document. That costs nothing — the probe already happens — so it is
//! parsed on every HEAD and the mirrors it names become reserves. See
//! [`mirrors_from_head`].
//!
//! # What is refused, and why
//!
//! * **Names that escape.** `<file name="../../etc/cron.d/x">` is a directory
//!   traversal written by whoever served the document, and a downloader that
//!   honours it writes an attacker-chosen path. RFC 5854 §4.1.2.1 forbids it;
//!   [`MetalinkFile::safe_name`] enforces it, including the Windows spellings
//!   (backslash, drive letter, ADS colon) that a POSIX-only check misses.
//! * **Schemes this build cannot fetch.** The Fedora document that motivated
//!   this module lists `rsync://` for six of its nineteen mirrors. They are
//!   parsed and kept — a reader should be able to *see* them — but
//!   [`MetaUrl::is_fetchable`] is false, so they never enter a source list.
//! * **`<metaurl>`.** BitTorrent and other indirections are recorded and not
//!   followed. Following one means implementing another transport, and quietly
//!   not following it while claiming to have used the document would be worse.

use crate::digest::Algo;
use crate::xml::{attr, Event, Reader};

/// Media type of a Metalink 4 document (RFC 5854 §5), i.e. a `.meta4`.
pub const MEDIA_TYPE_V4: &str = "application/metalink4+xml";
/// Media type of a Metalink 3 document, i.e. a `.metalink`.
pub const MEDIA_TYPE_V3: &str = "application/metalink+xml";

/// Namespace URI of Metalink 4 / RFC 5854.
pub const NS_V4: &str = "urn:ietf:params:xml:ns:metalink";
/// Namespace URI of Metalink 3.0.
pub const NS_V3: &str = "http://www.metalinker.org/";

/// Preference value meaning "no preference stated".
///
/// RFC 5854 §4.2.9 bounds `priority` at 999999 and says lower is better, so the
/// top of the range is the natural "unranked" value: an unranked mirror sorts
/// after every ranked one without needing a separate `Option` that every
/// comparison would have to unwrap.
pub const NO_PRIORITY: u32 = 999_999;

/// Largest document this parser will read, in bytes.
///
/// A mirror list is kilobytes. The cap exists because the document is fetched
/// from the network before anything about it is known, and an unbounded read of
/// an attacker-controlled body is a memory-exhaustion primitive regardless of
/// how careful the parser after it is.
pub const MAX_DOCUMENT: usize = 4 * 1024 * 1024;

/// Largest piece count accepted from a document.
///
/// `<hash piece="N">` indexes into a vector that is grown to fit, so an
/// unbounded `N` is an allocation the document chooses. One million pieces
/// covers a 4 TiB object at the smallest grid anyone publishes.
pub const MAX_PIECES: usize = 1_000_000;

/// Which dialect a document was written in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Version {
    /// Metalink 3.0, `http://www.metalinker.org/`.
    V3,
    /// Metalink 4.0, RFC 5854, `urn:ietf:params:xml:ns:metalink`.
    V4,
}

impl Version {
    pub fn as_str(self) -> &'static str {
        match self {
            Version::V3 => "3.0",
            Version::V4 => "4 (RFC 5854)",
        }
    }
}

/// Convert a Metalink 3.0 `preference` (0-100, higher better) to the RFC 5854
/// direction (1-999999, lower better).
///
/// This function is the whole reason the model has one preference field instead
/// of two. Getting the direction wrong is silent: the transfer still succeeds,
/// having given most of the work to the mirror the publisher ranked last.
pub fn priority_from_preference(preference: u32) -> u32 {
    // 100 -> 1 (best), 0 -> 101 (worst stated). Never 0: RFC 5854 starts at 1,
    // and reserving 0 keeps "unset" unambiguous.
    101 - preference.min(100)
}

/// What a URL points at, decided by its scheme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UrlKind {
    Http,
    Https,
    Ftp,
    /// A scheme this build has no transport for (`rsync`, `ftps`, `sftp`, ...).
    Unsupported(String),
}

impl UrlKind {
    pub fn from_url(url: &str) -> UrlKind {
        let scheme = url
            .split_once("://")
            .map(|(s, _)| s.to_ascii_lowercase())
            .unwrap_or_default();
        match scheme.as_str() {
            "http" => UrlKind::Http,
            "https" => UrlKind::Https,
            "ftp" => UrlKind::Ftp,
            other => UrlKind::Unsupported(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            UrlKind::Http => "http",
            UrlKind::Https => "https",
            UrlKind::Ftp => "ftp",
            UrlKind::Unsupported(s) => s,
        }
    }

    /// How good a TRANSPORT this scheme is for a ranged, multi-source
    /// transfer: 0 is best, larger is worse. Orthogonal to the publisher's
    /// ranking, which says how good the HOST is.
    ///
    /// # Why HTTP(S) outranks FTP by default
    ///
    /// The engine can splice byte ranges across HTTP mirrors, repair a corrupt
    /// chunk from a second one, and substitute a reserve mid-transfer. Over
    /// FTP it deliberately does none of that: range preemption costs
    /// control-channel round trips HTTP pays nothing for, so an FTP source is
    /// a single sequential stream. A publisher who ranks their FTP mirror
    /// first is ranking hosts, not transports — and following that ranking
    /// literally hands the transfer to the one scheme that turns off
    /// everything a mirror list is for. So the transport is the MAJOR sort
    /// key and the publisher's ranking orders mirrors within it: FTP mirrors
    /// stay listed, stay visible, and are used when they are all there is.
    pub fn transport_tier(&self) -> u8 {
        match self {
            UrlKind::Http | UrlKind::Https => 0,
            UrlKind::Ftp => 1,
            UrlKind::Unsupported(_) => 2,
        }
    }
}

/// One mirror.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetaUrl {
    pub url: String,
    /// RFC 5854 direction always: 1 is best, [`NO_PRIORITY`] means unranked.
    pub priority: u32,
    /// ISO 3166-1 alpha-2 country code, lowercased. `location` in 3.0, `location`
    /// in 4.0, `geo` in an RFC 6249 `Link` header.
    pub location: Option<String>,
    pub kind: UrlKind,
    /// Metalink 3.0 `maxconnections`, when the publisher stated one.
    ///
    /// Volunteer mirrors publish this and mean it. It is a per-source ceiling the
    /// politeness layer should not exceed even when the user asks for more
    /// connections, because the number came from the operator of the machine.
    pub max_connections: Option<usize>,
}

impl MetaUrl {
    /// Can this build actually fetch from this URL?
    ///
    /// False for `rsync://` and friends. They stay in the parsed document so a
    /// reader can see the full mirror list, and are filtered out of source lists.
    pub fn is_fetchable(&self) -> bool {
        !matches!(self.kind, UrlKind::Unsupported(_))
    }

    /// Sort key: stated priority first, then a stable tiebreak on the URL so two
    /// runs against the same document choose the same mirrors.
    fn key(&self) -> (u32, &str) {
        (self.priority, self.url.as_str())
    }
}

/// A whole-object digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetaHash {
    pub algo: Algo,
    /// Lowercase hex.
    pub hex: String,
}

impl MetaHash {
    /// `sha256:abc...`, the spelling `--checksum` and `ObjectMeta::digest` use.
    pub fn spec(&self) -> String {
        format!("{}:{}", self.algo.as_str(), self.hex)
    }
}

/// A per-chunk digest list: `<pieces length="..." type="...">`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pieces {
    /// Bytes per piece. Every piece but the last covers exactly this many.
    pub length: u64,
    pub algo: Algo,
    /// Lowercase hex, in object order.
    pub hashes: Vec<String>,
}

impl Pieces {
    /// Do these pieces describe an object of `size` bytes?
    ///
    /// Checked because a piece list of the wrong length is not a lesser
    /// manifest, it is a manifest for a different object: the grid would be
    /// applied at offsets the digests were never computed over, and every chunk
    /// would fail. Reporting "this document's pieces do not describe this file"
    /// is a diagnosis; reporting "all 471 chunks are corrupt" is not.
    pub fn covers(&self, size: u64) -> bool {
        self.length > 0 && size.div_ceil(self.length) as usize == self.hashes.len()
    }
}

/// An OpenPGP or S/MIME signature over the file, carried inline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    pub media_type: String,
    pub body: String,
}

/// One `<file>` entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetalinkFile {
    /// The `name` attribute VERBATIM, unvalidated. Use [`Self::safe_name`].
    pub name: String,
    pub size: Option<u64>,
    pub hashes: Vec<MetaHash>,
    pub pieces: Option<Pieces>,
    /// Mirrors, sorted best-first by [`MetaUrl::priority`].
    pub urls: Vec<MetaUrl>,
    /// `<metaurl>` indirections (BitTorrent and similar). Recorded, never followed.
    pub metaurls: Vec<MetaUrl>,
    pub signature: Option<Signature>,
    /// Metalink 3.0 `<resources maxconnections="N">`: the DEFAULT per-mirror
    /// ceiling for this file, inherited by every `<url>` that states none of its
    /// own.
    ///
    /// # Why the default, and not a ceiling across the whole file
    ///
    /// The 3.0 text is ambiguous and the two readings differ enormously. Read as
    /// an aggregate, Fedora's mirrormanager — which emits
    /// `<resources maxconnections="1">` on every document it generates, next to
    /// a list of seventeen mirrors — would be saying "use exactly one connection
    /// in total", which makes the seventeen mirrors it just published useful
    /// only as failover and makes a mirror list strictly worse than a URL. That
    /// cannot be what the publisher means, and it is not what the ecosystem
    /// does: `dnf`/`librepo`, the documents' primary consumer, fetches from
    /// several of those mirrors at once.
    ///
    /// Read as a per-mirror default it is coherent and conservative: one
    /// connection to each volunteer machine, seventeen machines, a real
    /// multi-source transfer that no single operator feels more than a single
    /// stream. That is the reading here, and [`MetaUrl::max_connections`]
    /// overrides it for any mirror that states its own.
    ///
    /// Applied during parsing, so a consumer never has to remember to: by the
    /// time a [`MetaUrl`] is handed out, its `max_connections` already carries
    /// whichever value governs it.
    pub default_max_connections: Option<usize>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub publisher: Option<String>,
    pub languages: Vec<String>,
    pub oses: Vec<String>,
}

/// Why a `name` attribute cannot be used as an output path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsafeName {
    pub raw: String,
    pub why: &'static str,
}

impl std::fmt::Display for UnsafeName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unsafe file name {:?}: {}", self.raw, self.why)
    }
}

impl MetalinkFile {
    /// The `name` attribute, if it is safe to use as a relative output path.
    ///
    /// # What this is defending against
    ///
    /// The name is chosen by whoever served the document, and it is used as a
    /// filesystem path. RFC 5854 §4.1.2.1 requires a relative path with no `..`
    /// element precisely because a client that resolves one writes wherever the
    /// document says — `../../.ssh/authorized_keys` from a mirror list is a
    /// remote write primitive, not a naming quirk.
    ///
    /// The Windows spellings are checked too, on every platform. A POSIX-only
    /// check passes `..\\..\\x`, `C:\\x` and `x:stream` through unharmed, and the
    /// document is not required to know which platform is reading it — so a
    /// cross-platform client that only rejects forward slashes is exploitable on
    /// the platform it was not tested on.
    pub fn safe_name(&self) -> Result<&str, UnsafeName> {
        let raw = self.name.as_str();
        let bad = |why: &'static str| {
            Err(UnsafeName {
                raw: raw.to_string(),
                why,
            })
        };
        if raw.is_empty() {
            return bad("empty");
        }
        if raw.len() > 255 {
            return bad("longer than 255 bytes");
        }
        if raw.contains('\0') {
            return bad("contains a NUL byte");
        }
        if raw.contains(|c: char| c.is_control()) {
            return bad("contains a control character");
        }
        // Absolute in either convention, or a UNC path.
        if raw.starts_with('/') || raw.starts_with('\\') {
            return bad("is an absolute path");
        }
        // `C:` — a drive-relative path on Windows, and an alternate data stream
        // separator anywhere later in the string.
        if raw.contains(':') {
            return bad("contains ':' (a Windows drive or alternate-data-stream separator)");
        }
        // Traversal, in either separator, at any position.
        let unified = raw.replace('\\', "/");
        if unified
            .split('/')
            .any(|seg| seg == ".." || seg.is_empty() || seg == ".")
        {
            return bad("contains a '.', '..' or empty path element");
        }
        // A percent-encoded separator is not decoded anywhere in this path, but a
        // caller downstream might, so it is refused rather than relied upon.
        let lower = raw.to_ascii_lowercase();
        if lower.contains("%2e") || lower.contains("%2f") || lower.contains("%5c") {
            return bad("contains a percent-encoded path separator or dot");
        }
        Ok(raw)
    }

    /// The base name alone, for a file whose `name` is a safe relative path.
    pub fn base_name(&self) -> Option<&str> {
        let n = self.safe_name().ok()?;
        n.rsplit(['/', '\\']).next().filter(|s| !s.is_empty())
    }

    /// The strongest whole-object digest published for this file.
    ///
    /// "Strongest" means cryptographic first (see [`Algo::is_cryptographic`]),
    /// then longest output. A document commonly lists md5, sha1, sha256 and
    /// sha512 together — verifying against the md5 when a sha512 was right there
    /// is a choice, and this is where it is made.
    pub fn best_hash(&self) -> Option<&MetaHash> {
        self.hashes.iter().max_by_key(|h| {
            (
                u8::from(h.algo.is_cryptographic()),
                match h.algo {
                    Algo::Sha512 => 4u8,
                    Algo::Sha256 => 3,
                    Algo::Sha1 => 2,
                    Algo::Md5 => 1,
                    Algo::Crc32c | Algo::Crc32 => 0,
                },
            )
        })
    }

    /// Mirrors this build can fetch from, best-first.
    pub fn fetchable_urls(&self) -> Vec<&MetaUrl> {
        self.urls.iter().filter(|u| u.is_fetchable()).collect()
    }
}

/// A parsed Metalink document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metalink {
    pub version: Option<Version>,
    pub files: Vec<MetalinkFile>,
    pub generator: Option<String>,
    pub published: Option<String>,
    /// `<origin>` (v4): where the authoritative copy of this document lives.
    pub origin: Option<String>,
}

impl Metalink {
    /// The file entry matching `name`, or the only entry when there is one.
    ///
    /// Matching is on the base name as well as the full relative path, because
    /// a user naming a file on the command line types what they see in a
    /// directory listing, not the document's internal path.
    pub fn file_named(&self, name: &str) -> Option<&MetalinkFile> {
        self.files
            .iter()
            .find(|f| f.name == name || f.base_name() == Some(name))
    }
}

/// Everything that can go wrong reading a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not XML, or not XML this parser accepts.
    Xml(String),
    /// Well-formed XML whose root element is not `<metalink>`.
    NotMetalink,
    /// A `<metalink>` root in a namespace this reader does not know.
    UnknownDialect(String),
    /// Structurally valid but with no `<file>` entries.
    Empty,
    TooLarge(usize),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Xml(e) => write!(f, "not a well-formed Metalink document: {e}"),
            Error::NotMetalink => write!(
                f,
                "the root element is not <metalink>; this is not a Metalink document"
            ),
            Error::UnknownDialect(ns) => write!(
                f,
                "<metalink> is in namespace {ns:?}, which is neither Metalink 3.0 \
                 ({NS_V3}) nor Metalink 4 ({NS_V4})"
            ),
            Error::Empty => write!(f, "the document lists no <file> entries"),
            Error::TooLarge(n) => write!(
                f,
                "the document is {n} bytes, over the {MAX_DOCUMENT}-byte limit for a mirror list"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Does this media type name a Metalink document?
pub fn is_metalink_media_type(ct: &str) -> bool {
    let base = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    base == MEDIA_TYPE_V4 || base == MEDIA_TYPE_V3
}

/// Does this filename look like a Metalink document?
pub fn is_metalink_filename(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    // The query string is not part of the name: mirrormanager URLs end in
    // `metalink?repo=fedora-40&arch=x86_64`, whose "extension" is `x86_64`.
    let n = n.split(['?', '#']).next().unwrap_or(&n);
    n.ends_with(".meta4") || n.ends_with(".metalink")
}

/// Does this body look like a Metalink document?
///
/// Content sniffing is a last resort and is treated as one: it is consulted only
/// when neither the media type nor the name settled the question. The check is
/// deliberately narrow — a root element named `metalink` near the head of the
/// body — because a false positive here means parsing a user's actual download
/// as a mirror list.
pub fn looks_like_metalink(body: &[u8]) -> bool {
    let head = &body[..body.len().min(4096)];
    let Ok(s) = std::str::from_utf8(&head[..head.len().min(4096)]) else {
        return false;
    };
    let lower = s.to_ascii_lowercase();
    lower.contains("<metalink") && (lower.contains(NS_V4) || lower.contains("metalinker.org"))
}

/// Parse a Metalink 3.0 or Metalink 4 document.
pub fn parse(src: &str) -> Result<Metalink, Error> {
    if src.len() > MAX_DOCUMENT {
        return Err(Error::TooLarge(src.len()));
    }
    let mut r = Reader::new(src);
    let mut out = Metalink::default();

    // Element path by local name, so a value is only accepted where it belongs.
    // `<hash>` means three different things depending on where it sits
    // (`file/hash`, `file/pieces/hash`, `file/verification/hash`), so a reader
    // that matched on the tag alone would mix whole-object digests into the
    // piece list and produce a manifest that fails on every chunk.
    let mut path: Vec<String> = Vec::new();
    let mut attrs_stack: Vec<Vec<crate::xml::Attr>> = Vec::new();
    let mut root_seen = false;

    loop {
        let ev = match r.read_event() {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(e) => return Err(Error::Xml(e.to_string())),
        };
        match ev {
            Event::Start {
                name,
                prefix: _,
                attrs,
            } => {
                if !root_seen {
                    root_seen = true;
                    if name != "metalink" {
                        return Err(Error::NotMetalink);
                    }
                    out.version = Some(dialect(&attrs)?);
                    out.generator = attr(&attrs, "generator").map(str::to_string);
                    out.published = attr(&attrs, "pubdate").map(str::to_string);
                    path.push(name);
                    attrs_stack.push(attrs);
                    continue;
                }
                if name == "file"
                    && matches!(path.last().map(String::as_str), Some("metalink" | "files"))
                {
                    out.files.push(MetalinkFile {
                        name: attr(&attrs, "name").unwrap_or_default().to_string(),
                        ..Default::default()
                    });
                }
                if let Some(f) = out.files.last_mut() {
                    start_in_file(f, &path, &name, &attrs);
                }
                path.push(name);
                attrs_stack.push(attrs);
            }
            Event::End { .. } => {
                path.pop();
                attrs_stack.pop();
            }
            Event::Text(t) => {
                let Some(f) = out.files.last_mut() else {
                    // `<generator>`, `<published>` and `<origin>` are ELEMENTS in
                    // v4 (RFC 5854 §4.2.3-4.2.5) where 3.0 spells the first two as
                    // attributes on the root. Both spellings are read, and the v4
                    // elements sit above the files, so they land here.
                    match path.last().map(String::as_str) {
                        Some("origin") => out.origin = Some(t.trim().to_string()),
                        Some("published") => out.published = Some(t.trim().to_string()),
                        Some("generator") => out.generator = Some(t.trim().to_string()),
                        _ => {}
                    }
                    continue;
                };
                let here = path.last().map(String::as_str).unwrap_or("");
                let attrs = attrs_stack.last().map(Vec::as_slice).unwrap_or(&[]);
                text_in_file(f, &path, here, attrs, &t);
            }
        }
    }

    if !root_seen {
        return Err(Error::NotMetalink);
    }
    if out.files.is_empty() {
        return Err(Error::Empty);
    }
    // Sort every mirror list best-first exactly once, here, so no consumer has to
    // remember to — and so `urls[0]` means "the publisher's first choice" rather
    // than "whatever came first in the file".
    for f in &mut out.files {
        // `<resources maxconnections>` is the default for the mirrors under it;
        // a `<url maxconnections>` overrides it. Pushed down here, once, so no
        // consumer has to remember which of the two governs a given mirror —
        // and forgetting would be silent, since both readings produce a working
        // transfer and only one of them respects the publisher.
        if let Some(d) = f.default_max_connections {
            for u in &mut f.urls {
                u.max_connections.get_or_insert(d);
            }
        }
        f.urls.sort_by(|a, b| a.key().cmp(&b.key()));
        f.metaurls.sort_by(|a, b| a.key().cmp(&b.key()));
    }
    Ok(out)
}

/// Decide the dialect from the root element's namespace.
///
/// The namespace, not the `version` attribute: mirrormanager emits
/// `version="3.0"` and Metalink 4 has no version attribute at all, so the
/// namespace is the only declaration both dialects actually make. A document
/// with neither namespace is refused rather than guessed at, because guessing
/// wrong inverts the preference scale.
fn dialect(attrs: &[crate::xml::Attr]) -> Result<Version, Error> {
    // The default namespace, or any `xmlns:*` binding — a prefixed document
    // declares the metalink URI on a prefix rather than on `xmlns`.
    let mut ns: Option<&str> = None;
    for a in attrs {
        let is_ns =
            (a.name == "xmlns" && a.prefix.is_none()) || a.prefix.as_deref() == Some("xmlns");
        if !is_ns {
            continue;
        }
        let v = a.value.trim().trim_end_matches('/');
        if v == NS_V4 {
            return Ok(Version::V4);
        }
        if v == NS_V3.trim_end_matches('/') {
            return Ok(Version::V3);
        }
        if ns.is_none() && a.name == "xmlns" {
            ns = Some(a.value.as_str());
        }
    }
    match ns {
        Some(other) => Err(Error::UnknownDialect(other.to_string())),
        // No namespace at all. Both dialects require one, but a hand-written or
        // stripped document is more useful read than refused, and `version="3.0"`
        // is the only remaining hint. Defaulting to 4 would invert the preference
        // scale on exactly the documents most likely to be malformed 3.0.
        None => Ok(
            match attrs
                .iter()
                .find(|a| a.name == "version")
                .map(|a| a.value.trim())
            {
                Some(v) if v.starts_with('3') => Version::V3,
                _ => Version::V4,
            },
        ),
    }
}

/// Handle an element that carries its payload in ATTRIBUTES.
fn start_in_file(f: &mut MetalinkFile, path: &[String], name: &str, attrs: &[crate::xml::Attr]) {
    let parent = path.last().map(String::as_str).unwrap_or("");
    match name {
        // v4 `<pieces length="262144" type="sha-256">`,
        // v3 `<pieces length="262144" type="sha1">`.
        "pieces" => {
            let length = attr(attrs, "length")
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let algo = attr(attrs, "type").and_then(Algo::parse);
            if let (true, Some(algo)) = (length > 0, algo) {
                f.pieces = Some(Pieces {
                    length,
                    algo,
                    hashes: Vec::new(),
                });
            }
        }
        "signature" => {
            f.signature = Some(Signature {
                media_type: attr(attrs, "mediatype")
                    .or_else(|| attr(attrs, "type"))
                    .unwrap_or("application/pgp-signature")
                    .to_string(),
                body: String::new(),
            });
        }
        // v3 `<resources maxconnections="N">`: the default per-mirror ceiling.
        "resources" => {
            f.default_max_connections = attr(attrs, "maxconnections")
                .and_then(|v| v.trim().parse::<usize>().ok())
                .filter(|&n| n > 0);
        }
        // v3 keeps the size as an attribute on some emitters as well as an element.
        "file" if parent == "metalink" || parent == "files" => {
            if let Some(n) = attr(attrs, "size").and_then(|v| v.trim().parse::<u64>().ok()) {
                f.size = Some(n);
            }
        }
        _ => {}
    }
}

/// Handle an element whose payload is TEXT, keyed on where it sits.
fn text_in_file(
    f: &mut MetalinkFile,
    path: &[String],
    here: &str,
    attrs: &[crate::xml::Attr],
    text: &str,
) {
    let t = text.trim();
    if t.is_empty() {
        return;
    }
    let parent = path
        .len()
        .checked_sub(2)
        .and_then(|i| path.get(i))
        .map(String::as_str)
        .unwrap_or("");
    match here {
        "size" => f.size = t.parse::<u64>().ok(),
        "version" => f.version = Some(t.to_string()),
        "description" => f.description = Some(t.to_string()),
        "publisher" | "name" if parent == "publisher" => f.publisher = Some(t.to_string()),
        "language" => f.languages.push(t.to_ascii_lowercase()),
        "os" => f.oses.push(t.to_ascii_lowercase()),
        "signature" => {
            if let Some(s) = &mut f.signature {
                s.body = text.to_string();
            }
        }
        // The three meanings of `<hash>`, told apart by the parent.
        "hash" if parent == "pieces" => {
            let Some(p) = &mut f.pieces else { return };
            // Validated, not merely lowercased. A piece digest of the wrong
            // length is unusable, and storing it anyway defers the discovery to
            // chunk-verification time, where it presents as "this chunk is
            // corrupt" about bytes that are fine. Poisoning the whole list is
            // right: a grid missing one digest cannot verify the object.
            let Some(hex) = crate::digest::to_hex(t, p.algo) else {
                f.pieces = None;
                return;
            };
            // v3 numbers its pieces with `piece="N"` and does NOT guarantee
            // document order; v4 relies on order alone. Honour the index when it
            // is stated, so an out-of-order document produces the right grid
            // rather than a correctly-sized grid whose digests are permuted —
            // which fails every chunk and reads as corruption.
            match attr(attrs, "piece").and_then(|v| v.trim().parse::<usize>().ok()) {
                Some(i) if i < MAX_PIECES => {
                    if p.hashes.len() <= i {
                        p.hashes.resize(i + 1, String::new());
                    }
                    p.hashes[i] = hex;
                }
                Some(_) => f.pieces = None,
                None => p.hashes.push(hex),
            }
        }
        "hash" => {
            // v4: `file/hash`. v3: `file/verification/hash`. Both carry `type`.
            if let Some(algo) = attr(attrs, "type").and_then(Algo::parse) {
                if let Some(hex) = crate::digest::to_hex(t, algo) {
                    f.hashes.push(MetaHash { algo, hex });
                }
            }
        }
        "url" => {
            if let Some(u) = read_url(attrs, t) {
                f.urls.push(u);
            }
        }
        "metaurl" => {
            if let Some(u) = read_url(attrs, t) {
                f.metaurls.push(u);
            }
        }
        _ => {}
    }
}

/// Build a [`MetaUrl`] from a `<url>`/`<metaurl>` element.
///
/// This is where the two preference scales are reconciled. `priority` (v4) is
/// taken as written; `preference` (v3) is inverted. A document that somehow
/// carries both is read as v4, since `priority` is the newer and more precise of
/// the two.
fn read_url(attrs: &[crate::xml::Attr], text: &str) -> Option<MetaUrl> {
    let url = text.trim();
    if url.is_empty() || !url.contains("://") {
        return None;
    }
    let priority = attr(attrs, "priority")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .map(|p| p.clamp(1, NO_PRIORITY))
        .or_else(|| {
            attr(attrs, "preference")
                .and_then(|v| v.trim().parse::<u32>().ok())
                .map(priority_from_preference)
        })
        .unwrap_or(NO_PRIORITY);
    Some(MetaUrl {
        kind: UrlKind::from_url(url),
        url: url.to_string(),
        priority,
        location: attr(attrs, "location")
            .map(|l| l.trim().to_ascii_lowercase())
            .filter(|l| !l.is_empty()),
        max_connections: attr(attrs, "maxconnections")
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&n| n > 0),
    })
}

// ---------------------------------------------------------------------------
// Metalink over HTTP (RFC 6249)
// ---------------------------------------------------------------------------

/// One parsed `Link:` header field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkField {
    pub url: String,
    /// `rel` parameter, lowercased. `duplicate` names a mirror; `describedby`
    /// names a document about the object.
    pub rel: Option<String>,
    /// `pri` parameter: RFC 6249 §3.2, same direction as RFC 5854 (lower better).
    pub pri: Option<u32>,
    /// `geo` parameter: ISO 3166-1 alpha-2, lowercased.
    pub geo: Option<String>,
    /// `type` parameter, lowercased.
    pub media_type: Option<String>,
}

/// Parse every `Link:` field out of a response header block.
///
/// One header line may carry several comma-separated link-values, and a response
/// may repeat the header — both forms occur, and a reader that handles only one
/// of them silently loses mirrors.
pub fn links_from_head(head: &str) -> Vec<LinkField> {
    let mut out = Vec::new();
    for line in head.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("link") {
            continue;
        }
        for lv in split_link_values(value) {
            if let Some(l) = parse_link_value(&lv) {
                out.push(l);
            }
        }
    }
    out
}

/// Split a `Link` field value on commas that separate link-values.
///
/// A comma inside `<...>` or inside a quoted parameter does not separate: real
/// mirror URLs carry query strings, and `type="a,b"` is legal. Splitting on every
/// comma turns one mirror into two malformed ones.
fn split_link_values(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut depth, mut quoted, mut start) = (0i32, false, 0usize);
    for (i, c) in v.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '<' if !quoted => depth += 1,
            '>' if !quoted => depth -= 1,
            ',' if !quoted && depth <= 0 => {
                out.push(v[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(v[start..].to_string());
    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_link_value(s: &str) -> Option<LinkField> {
    let s = s.trim();
    let open = s.find('<')?;
    let close = s[open + 1..].find('>')? + open + 1;
    let url = s[open + 1..close].trim().to_string();
    if url.is_empty() {
        return None;
    }
    let mut f = LinkField {
        url,
        rel: None,
        pri: None,
        geo: None,
        media_type: None,
    };
    for param in s[close + 1..].split(';') {
        let Some((k, v)) = param.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"').trim();
        match k.trim().to_ascii_lowercase().as_str() {
            "rel" => f.rel = Some(v.to_ascii_lowercase()),
            "pri" => f.pri = v.parse::<u32>().ok(),
            "geo" => f.geo = Some(v.to_ascii_lowercase()),
            "type" => f.media_type = Some(v.to_ascii_lowercase()),
            _ => {}
        }
    }
    Some(f)
}

/// Mirrors advertised by `Link: <...>; rel=duplicate` (RFC 6249 §3), best-first.
///
/// # Why this is worth parsing on every probe
///
/// It costs nothing. The HEAD already happened, the header is already in
/// `Probe::raw_head`, and a server that implements RFC 6249 is telling the client
/// exactly what it needs to survive that server dying mid-transfer. A download
/// from one URL becomes a download with reserves, with no extra request and no
/// flag to remember.
pub fn mirrors_from_head(head: &str) -> Vec<MetaUrl> {
    let mut v: Vec<MetaUrl> = links_from_head(head)
        .into_iter()
        .filter(|l| l.rel.as_deref() == Some("duplicate"))
        .map(|l| MetaUrl {
            kind: UrlKind::from_url(&l.url),
            url: l.url,
            priority: l
                .pri
                .map(|p| p.clamp(1, NO_PRIORITY))
                .unwrap_or(NO_PRIORITY),
            location: l.geo,
            max_connections: None,
        })
        .collect();
    v.sort_by(|a, b| a.key().cmp(&b.key()));
    v
}

/// The URL of a Metalink document describing this object, if the response named
/// one with `rel=describedby` and a Metalink media type.
pub fn describedby_metalink(head: &str) -> Option<String> {
    links_from_head(head)
        .into_iter()
        .find(|l| {
            l.rel.as_deref() == Some("describedby")
                && l.media_type.as_deref().is_some_and(is_metalink_media_type)
        })
        .map(|l| l.url)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The document in the request that motivated this module, trimmed to three
    /// mirrors of each interesting kind. Real mirrormanager output: Metalink 3.0,
    /// a foreign-namespace element, rsync URLs mixed in, `maxconnections`.
    const FEDORA_V3: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<metalink version="3.0" xmlns="http://www.metalinker.org/" type="dynamic"
          pubdate="Sat, 22 Aug 2026 20:45:45 GMT" generator="mirrormanager"
          xmlns:mm0="http://fedorahosted.org/mirrormanager">
 <files>
  <file name="repomd.xml">
   <mm0:timestamp>1713120671</mm0:timestamp>
   <size>6285</size>
   <verification>
    <hash type="md5">8a9923bd9faba440fbe2c8ea5c5b301e</hash>
    <hash type="sha1">8aed8f72da845069152236f2df03ea0b77c6ad56</hash>
    <hash type="sha256">d201bd1eeb17086cd3aaf82b156810a5ba3f389e10b4472c9b2c7182f771a9ef</hash>
   </verification>
   <resources maxconnections="1">
    <url protocol="https" type="https" location="UA" preference="100">https://a.example/repomd.xml</url>
    <url protocol="rsync" type="rsync" location="UA" preference="100">rsync://a.example/repomd.xml</url>
    <url protocol="https" type="https" location="DE" preference="99">https://b.example/repomd.xml</url>
    <url protocol="http" type="http" location="US" preference="93">http://c.example/repomd.xml</url>
   </resources>
  </file>
 </files>
</metalink>"#;

    const META4: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <generator>hydra-test/1.0</generator>
  <published>2026-08-22T20:45:45Z</published>
  <origin dynamic="true">https://example.org/big.iso.meta4</origin>
  <file name="big.iso">
    <size>4194304</size>
    <hash type="sha-256">d201bd1eeb17086cd3aaf82b156810a5ba3f389e10b4472c9b2c7182f771a9ef</hash>
    <pieces length="1048576" type="sha-256">
      <hash>0000000000000000000000000000000000000000000000000000000000000001</hash>
      <hash>0000000000000000000000000000000000000000000000000000000000000002</hash>
      <hash>0000000000000000000000000000000000000000000000000000000000000003</hash>
      <hash>0000000000000000000000000000000000000000000000000000000000000004</hash>
    </pieces>
    <url priority="1" location="de">https://de.example/big.iso</url>
    <url priority="9" location="us">https://us.example/big.iso</url>
    <metaurl mediatype="torrent" priority="2">https://example.org/big.iso.torrent</metaurl>
  </file>
</metalink>"#;

    #[test]
    fn the_two_preference_scales_run_in_opposite_directions() {
        // The defect this whole model exists to prevent. In 3.0 the BEST mirror
        // has the LARGEST number; in 4 it has the smallest. A reader that treats
        // them as one number gives most of the work to the worst mirror, and the
        // transfer still succeeds — just slower, indistinguishably from bad luck.
        let v3 = parse(FEDORA_V3).unwrap();
        assert_eq!(v3.version, Some(Version::V3));
        let f = &v3.files[0];
        // preference=100 became the best priority, preference=93 the worst.
        assert_eq!(f.urls[0].url, "https://a.example/repomd.xml");
        assert_eq!(f.urls[0].priority, priority_from_preference(100));
        assert_eq!(f.urls[0].priority, 1);
        let last = f.urls.last().unwrap();
        assert_eq!(last.priority, priority_from_preference(93));
        assert!(last.priority > f.urls[0].priority);

        let v4 = parse(META4).unwrap();
        assert_eq!(v4.version, Some(Version::V4));
        assert_eq!(v4.files[0].urls[0].priority, 1);
        assert_eq!(v4.files[0].urls[0].url, "https://de.example/big.iso");
    }

    #[test]
    fn the_transport_tier_orders_schemes_by_what_the_engine_can_do_with_them() {
        // 0 is the tier the range engine can splice and repair; ftp streams
        // from one connection; an unsupported scheme cannot be fetched at all.
        assert_eq!(UrlKind::Http.transport_tier(), 0);
        assert_eq!(UrlKind::Https.transport_tier(), 0);
        assert_eq!(UrlKind::Ftp.transport_tier(), 1);
        assert!(
            UrlKind::Unsupported("rsync".into()).transport_tier() > UrlKind::Ftp.transport_tier()
        );
    }

    #[test]
    fn unsupported_schemes_are_visible_but_not_fetchable() {
        // Six of the nineteen mirrors in the real document are rsync. Keeping
        // them lets a reader see the full list; `is_fetchable` keeps them out of
        // the source list, where they would each cost a failed connection.
        let m = parse(FEDORA_V3).unwrap();
        let f = &m.files[0];
        assert_eq!(f.urls.len(), 4);
        assert!(f.urls.iter().any(|u| !u.is_fetchable()));
        assert_eq!(f.fetchable_urls().len(), 3);
        assert!(f.fetchable_urls().iter().all(|u| u.is_fetchable()));
        let rs = f.urls.iter().find(|u| !u.is_fetchable()).unwrap();
        assert_eq!(rs.kind, UrlKind::Unsupported("rsync".into()));
    }

    #[test]
    fn a_foreign_namespace_element_does_not_disturb_the_walk() {
        // `<mm0:timestamp>` sits between `<file>` and `<size>` in real
        // mirrormanager output.
        let m = parse(FEDORA_V3).unwrap();
        assert_eq!(m.files[0].size, Some(6285));
        // 3.0 spells the generator as a root ATTRIBUTE; RFC 5854 spells it as an
        // ELEMENT (§4.2.3). Reading only one of the two leaves half the documents
        // in the wild reporting no generator at all.
        assert_eq!(m.generator.as_deref(), Some("mirrormanager"));
        let v4 = parse(META4).unwrap();
        assert_eq!(v4.generator.as_deref(), Some("hydra-test/1.0"));
        assert_eq!(v4.published.as_deref(), Some("2026-08-22T20:45:45Z"));
        assert_eq!(
            v4.origin.as_deref(),
            Some("https://example.org/big.iso.meta4")
        );
    }

    #[test]
    fn the_strongest_digest_is_chosen_when_several_are_published() {
        // The real document lists md5, sha1, sha256 and sha512 together.
        // Verifying against the md5 with a sha256 in hand is a choice, and it is
        // the wrong one.
        let m = parse(FEDORA_V3).unwrap();
        let f = &m.files[0];
        assert_eq!(f.hashes.len(), 3);
        let best = f.best_hash().unwrap();
        assert_eq!(best.algo, Algo::Sha256);
        assert!(best.spec().starts_with("sha256:d201bd1e"));
    }

    #[test]
    fn a_resources_ceiling_is_the_default_for_its_mirrors_not_a_cap_on_the_file() {
        // The reading that matters. Fedora's mirrormanager emits
        // `<resources maxconnections="1">` on EVERY document, beside a list of
        // seventeen mirrors. As an aggregate that says "one connection in
        // total", which makes the seventeen mirrors it just published useless
        // and a mirror list strictly worse than a plain URL. As a per-mirror
        // default it says "one connection to each volunteer machine", which is
        // coherent, conservative, and what `dnf` does with the same documents.
        let m = parse(FEDORA_V3).unwrap();
        assert_eq!(m.files[0].default_max_connections, Some(1));
        assert!(
            m.files[0].urls.iter().all(|u| u.max_connections == Some(1)),
            "every mirror inherits the default"
        );

        // A mirror that states its own overrides the default; one that does not
        // inherits it. Applied during parsing so no consumer has to remember
        // which of the two governs a given mirror.
        let src = r#"<metalink version="3.0" xmlns="http://www.metalinker.org/"><files>
          <file name="f"><size>1</size><resources maxconnections="6">
            <url maxconnections="2">https://a.example/f</url>
            <url>https://b.example/f</url>
          </resources></file></files></metalink>"#;
        let m = parse(src).unwrap();
        assert_eq!(m.files[0].default_max_connections, Some(6));
        let by_url = |u: &str| {
            m.files[0]
                .urls
                .iter()
                .find(|x| x.url == u)
                .unwrap()
                .max_connections
        };
        assert_eq!(by_url("https://a.example/f"), Some(2), "its own wins");
        assert_eq!(
            by_url("https://b.example/f"),
            Some(6),
            "the default applies"
        );

        // With no `<resources maxconnections>` at all, nothing is invented: the
        // client's own per-host setting governs.
        let bare = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="f">
            <size>1</size><url>https://a/f</url></file></metalink>"#;
        let m = parse(bare).unwrap();
        assert_eq!(m.files[0].default_max_connections, None);
        assert_eq!(m.files[0].urls[0].max_connections, None);
    }

    #[test]
    fn pieces_become_a_chunk_grid_and_are_checked_against_the_size() {
        let m = parse(META4).unwrap();
        let f = &m.files[0];
        let p = f.pieces.as_ref().unwrap();
        assert_eq!(p.length, 1024 * 1024);
        assert_eq!(p.algo, Algo::Sha256);
        assert_eq!(p.hashes.len(), 4);
        assert!(p.covers(4 * 1024 * 1024));
        // A grid that does not tile the object is a manifest for a DIFFERENT
        // object; applying it would fail every chunk and report corruption where
        // the real fault is a mismatched document.
        assert!(!p.covers(4 * 1024 * 1024 + 1));
        assert!(!p.covers(1024));
    }

    #[test]
    fn v3_numbers_its_pieces_and_the_index_is_honoured_over_document_order() {
        // Metalink 3.0 writes `piece="N"` and does not promise to emit them in
        // order. Trusting order would build a correctly-sized grid whose digests
        // are permuted — every chunk fails, and the reported fault is corruption
        // rather than a shuffled document.
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let c = "c".repeat(40);
        let src = format!(
            r#"<metalink version="3.0" xmlns="http://www.metalinker.org/"><files>
          <file name="f"><size>3</size><verification>
            <pieces type="sha1" length="1">
              <hash piece="1">{b}</hash>
              <hash piece="0">{a}</hash>
              <hash piece="2">{c}</hash>
            </pieces></verification></file></files></metalink>"#
        );
        let m = parse(&src).unwrap();
        let p = m.files[0].pieces.as_ref().unwrap();
        assert_eq!(p.hashes, vec![a, b, c]);
        assert!(p.covers(3));
    }

    #[test]
    fn a_piece_digest_of_the_wrong_length_poisons_the_grid_rather_than_the_chunk() {
        // Storing it defers the discovery to verification time, where a document
        // defect presents as "this chunk is corrupt" about bytes that are fine.
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="f">
            <size>2</size>
            <pieces length="1" type="sha-256">
              <hash>0000000000000000000000000000000000000000000000000000000000000001</hash>
              <hash>deadbeef</hash>
            </pieces></file></metalink>"#;
        let m = parse(src).unwrap();
        assert!(m.files[0].pieces.is_none());
    }

    #[test]
    fn whole_object_and_piece_hashes_are_not_mixed() {
        // `<hash>` means three different things depending on where it sits. A
        // reader matching on the tag alone folds the object digest into the piece
        // list, which then fails to tile and takes the piece verification down
        // with it.
        let m = parse(META4).unwrap();
        let f = &m.files[0];
        assert_eq!(f.hashes.len(), 1, "one whole-object digest");
        assert_eq!(f.pieces.as_ref().unwrap().hashes.len(), 4);
        assert!(!f.pieces.as_ref().unwrap().hashes.contains(&f.hashes[0].hex));
    }

    #[test]
    fn metaurls_are_recorded_and_kept_out_of_the_mirror_list() {
        // Following one means implementing BitTorrent. Silently ignoring it while
        // claiming to have used the document would be worse than recording it.
        let m = parse(META4).unwrap();
        let f = &m.files[0];
        assert_eq!(f.metaurls.len(), 1);
        assert!(f.urls.iter().all(|u| !u.url.ends_with(".torrent")));
    }

    #[test]
    fn a_name_that_escapes_the_output_directory_is_refused() {
        // The name is chosen by whoever served the document and is used as a
        // path. RFC 5854 §4.1.2.1 forbids traversal for this reason.
        for bad in [
            "../../etc/passwd",
            "/etc/passwd",
            "a/../../b",
            "..\\..\\windows\\system32\\x",
            "\\\\server\\share\\x",
            "C:\\x",
            "file:stream",
            "a/./b",
            "",
            "with%2e%2e/slash",
            "nul\u{0}byte",
        ] {
            let f = MetalinkFile {
                name: bad.into(),
                ..Default::default()
            };
            assert!(
                f.safe_name().is_err(),
                "{bad:?} must not be usable as an output path"
            );
        }
        for ok in ["repomd.xml", "releases/40/big.iso", "a-b_c.1.tar.zst"] {
            let f = MetalinkFile {
                name: ok.into(),
                ..Default::default()
            };
            assert_eq!(f.safe_name().unwrap(), ok);
        }
        let f = MetalinkFile {
            name: "releases/40/big.iso".into(),
            ..Default::default()
        };
        assert_eq!(f.base_name(), Some("big.iso"));
    }

    #[test]
    fn a_document_that_is_not_a_metalink_is_refused_with_a_reason() {
        assert_eq!(
            parse("<html><body>hi</body></html>"),
            Err(Error::NotMetalink)
        );
        assert!(matches!(parse("not xml at all"), Err(Error::NotMetalink)));
        assert!(matches!(
            parse(r#"<metalink xmlns="http://example.com/other"><file name="a"/></metalink>"#),
            Err(Error::UnknownDialect(_))
        ));
        assert_eq!(
            parse(r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"></metalink>"#),
            Err(Error::Empty)
        );
        let huge = "x".repeat(MAX_DOCUMENT + 1);
        assert!(matches!(parse(&huge), Err(Error::TooLarge(_))));
    }

    #[test]
    fn a_truncated_document_does_not_parse_as_a_shorter_mirror_list() {
        // The realistic failure: the fetch is cut off mid-document. A reader that
        // accepts the prefix builds a source list out of whichever mirrors
        // happened to arrive, and reports success.
        let cut = &META4[..META4.len() / 2];
        assert!(matches!(parse(cut), Err(Error::Xml(_))));
    }

    #[test]
    fn media_type_and_filename_detection() {
        assert!(is_metalink_media_type("application/metalink4+xml"));
        assert!(is_metalink_media_type(
            "application/metalink+xml; charset=utf-8"
        ));
        assert!(!is_metalink_media_type("application/xml"));
        assert!(is_metalink_filename("big.iso.meta4"));
        assert!(is_metalink_filename("fedora.METALINK"));
        // mirrormanager's URL: the last dotted token is `x86_64`, not an
        // extension, so the query string must be cut before the suffix test.
        assert!(!is_metalink_filename("metalink?repo=fedora-40&arch=x86_64"));
        assert!(looks_like_metalink(FEDORA_V3.as_bytes()));
        assert!(looks_like_metalink(META4.as_bytes()));
        assert!(!looks_like_metalink(b"<html><metalink-ish></html>"));
        assert!(!looks_like_metalink(&[0xff, 0xd8, 0xff, 0xe0]));
    }

    #[test]
    fn rfc6249_link_headers_yield_mirrors_and_a_document_url() {
        let head = "HTTP/1.1 200 OK\r\n\
            Content-Length: 100\r\n\
            Link: <http://mirror1.example/f>; rel=duplicate; pri=1; geo=de, \
                  <http://mirror2.example/f?a=1,2>; rel=duplicate; pri=5\r\n\
            Link: <http://example.org/f.meta4>; rel=describedby; \
                  type=\"application/metalink4+xml\"\r\n\
            \r\n";
        let ms = mirrors_from_head(head);
        assert_eq!(ms.len(), 2);
        assert_eq!(ms[0].url, "http://mirror1.example/f");
        assert_eq!(ms[0].priority, 1);
        assert_eq!(ms[0].location.as_deref(), Some("de"));
        // A comma inside a query string does not separate two link-values.
        assert_eq!(ms[1].url, "http://mirror2.example/f?a=1,2");
        assert_eq!(
            describedby_metalink(head).as_deref(),
            Some("http://example.org/f.meta4")
        );
        // A describedby that is not a Metalink is not one.
        let other = "Link: <http://x/y.html>; rel=describedby; type=\"text/html\"\r\n";
        assert_eq!(describedby_metalink(other), None);
        assert!(mirrors_from_head("HTTP/1.1 200 OK\r\n\r\n").is_empty());
    }

    #[test]
    fn an_unranked_mirror_sorts_after_every_ranked_one() {
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="f">
            <url>https://unranked.example/f</url>
            <url priority="7">https://seven.example/f</url>
            <url priority="1">https://one.example/f</url>
          </file></metalink>"#;
        let m = parse(src).unwrap();
        let got: Vec<&str> = m.files[0].urls.iter().map(|u| u.url.as_str()).collect();
        assert_eq!(
            got,
            vec![
                "https://one.example/f",
                "https://seven.example/f",
                "https://unranked.example/f"
            ]
        );
        assert_eq!(m.files[0].urls[2].priority, NO_PRIORITY);
    }

    #[test]
    fn entity_encoded_query_strings_survive_into_the_url() {
        // `&` must be written `&amp;` in XML, and mirror URLs are full of query
        // strings. A URL with the entity left in it 404s.
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="f">
            <url>https://h.example/get?repo=fedora-40&amp;arch=x86_64</url>
          </file></metalink>"#;
        let m = parse(src).unwrap();
        assert_eq!(
            m.files[0].urls[0].url,
            "https://h.example/get?repo=fedora-40&arch=x86_64"
        );
    }

    #[test]
    fn several_files_are_kept_and_selectable_by_either_name_form() {
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">
            <file name="dir/a.bin"><url>https://h/a</url></file>
            <file name="b.bin"><url>https://h/b</url></file>
          </metalink>"#;
        let m = parse(src).unwrap();
        assert_eq!(m.files.len(), 2);
        assert_eq!(m.file_named("b.bin").unwrap().name, "b.bin");
        // A user types the name they see in a listing, not the internal path.
        assert_eq!(m.file_named("a.bin").unwrap().name, "dir/a.bin");
        assert_eq!(m.file_named("dir/a.bin").unwrap().name, "dir/a.bin");
        assert!(m.file_named("nope").is_none());
    }
}
