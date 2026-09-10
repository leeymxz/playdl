//! Turning a Metalink document into jobs the engine can run.
//!
//! [`pdl_net::metalink`] does the reading; this module does the *choosing*. Those
//! are genuinely different problems and keeping them apart is what lets the
//! parser stay a pure function of the bytes: a document lists every mirror the
//! publisher knows about, in every protocol, for every platform, and a run wants
//! a handful of them for one file on one machine.
//!
//! # What the choosing consists of
//!
//! 1. **Which file.** A document may describe many. `--metalink-file` names one;
//!    `--metalink-language`, `--metalink-os` and `--metalink-version` filter by
//!    what the entries say about themselves.
//! 2. **Which mirrors, in what order.** Location preference, protocol
//!    preference, and the publisher's own ranking, resolved into a single
//!    ordering â€?see [`rank`].
//! 3. **Which are sources and which are reserves.** Politeness authorises a
//!    handful of connections; everything past that is a bench for
//!    [`pdl_net::run_transfer_with_reserves`] to draw on. This is where a
//!    nineteen-mirror list stops being decoration.
//! 4. **What the bytes must hash to**, and â€?when the document publishes
//!    `<pieces>` â€?what each 256 KiB or 1 MiB of them must hash to, which is what
//!    turns a corrupt download into a corrupt *chunk*.
//!
//! # The one thing a Metalink changes about correctness
//!
//! Mirror assembly is normally gated on every source agreeing about a strong
//! validator, because two mirrors that serve different builds produce a file
//! that passes every length check and is silently wrong. That gate is
//! unsatisfiable across a real mirror list: independent operators running
//! independent web servers do not, and cannot, share an `ETag`. So `--mirrors`
//! against a genuine mirror list keeps exactly one source, which is the correct
//! answer to the question it was asking and useless for the thing the user
//! wanted.
//!
//! A Metalink answers a better question. It states the size and a content digest
//! for the object, from a host that is usually not any of the mirrors, so
//! agreement is established against the *document* instead of pairwise between
//! servers. A mirror is admitted if it agrees with the document's size, and the
//! digest catches anything that slipped through â€?and with `<pieces>`, catches
//! it per chunk and repairs it from a different mirror. That is a stronger
//! guarantee than matching ETags, not a weaker one, and it is why
//! [`Attested`] exists.

use pdl_core::SourcePlan;
use pdl_net::metalink::{self, MetaUrl, Metalink, MetalinkFile};
use std::path::Path;
use std::sync::Arc;

/// Largest Metalink document fetched over the network.
const FETCH_CAP: usize = pdl_net::metalink::MAX_DOCUMENT;

/// What the user asked for on the command line.
#[derive(Clone, Debug, Default)]
pub struct Selection {
    /// `--metalink-file`: which entry to take. `None` means every entry.
    pub file: Option<String>,
    /// `--metalink-location`: preferred locations, best first.
    pub locations: Vec<String>,
    /// `--metalink-language`.
    pub language: Option<String>,
    /// `--metalink-os`.
    pub os: Option<String>,
    /// `--metalink-version`.
    pub version: Option<String>,
    /// `--metalink-preferred-protocol`: `http`, `https`, `ftp`, or `none`.
    pub preferred_protocol: Option<String>,
    /// `--metalink-enable-unique-protocol`: use only one protocol for a file.
    pub unique_protocol: bool,
}

/// Size and digests attested by a document, rather than by a mirror.
#[derive(Clone, Debug)]
pub struct Attested {
    pub size: u64,
    /// `sha256:...`, the strongest digest the document published.
    pub digest: Option<String>,
    /// Per-chunk digests, when the document published `<pieces>`.
    pub pieces: Option<pdl_net::manifest::Manifest>,
    /// Where the document came from, for the report.
    pub origin: String,
}

/// One file from a document, ready to become a [`crate::download::Job`].
#[derive(Clone, Debug)]
pub struct Resolved {
    /// Safe relative path from the document's `name`.
    pub name: String,
    /// Mirrors to fetch from, best first.
    pub urls: Vec<String>,
    /// Ranking and per-mirror ceilings, index-aligned with `urls`.
    pub plans: Vec<SourcePlan>,
    pub attested: Option<Attested>,
    /// Things worth telling the user: mirrors dropped, digests unavailable,
    /// pieces that did not tile.
    pub notes: Vec<String>,
    /// `<signature>` was present. Recorded, not verified â€?see [`Resolved::signature_note`].
    pub signed: bool,
}

impl Resolved {
    /// What to say about a `<signature>` this build cannot check.
    ///
    /// Silence would be the wrong answer in both directions: a user who sees no
    /// mention assumes there was nothing to check, and a user who sees the
    /// signature reported without qualification assumes it was verified. Neither
    /// is true, so the note says exactly what happened.
    pub fn signature_note(&self) -> Option<&'static str> {
        self.signed.then_some(
            "the document carries an OpenPGP signature over this file; hydra records it \
             but does not verify it â€?verify it yourself before trusting the digests it \
             covers",
        )
    }
}

/// Where a document came from.
#[derive(Clone, Debug)]
pub enum Origin {
    File(std::path::PathBuf),
    Url(String),
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Origin::File(p) => write!(f, "{}", p.display()),
            Origin::Url(u) => write!(f, "{u}"),
        }
    }
}

/// Does this command-line argument name a Metalink document?
///
/// # Three places the answer can come from, and what each costs
///
/// Detection is automatic in all three, which is why `--metalink` is an override
/// rather than a requirement: a user who has a mirror list should be able to
/// point hydra at it and have it work.
///
/// 1. **The name.** `.meta4` and `.metalink`. Free, and the only signal a remote
///    URL offers before it is fetched.
/// 2. **The content**, for a LOCAL path only. One 4 KiB read of a file already
///    on disk, which is cheap and certain â€?and necessary, because a document
///    saved by a browser is as likely to be called `metalink.xml` or
///    `download(1)` as anything else.
/// 3. **The `Content-Type`**, for a remote URL. Not consulted here: it needs the
///    probe, which happens inside the engine. `https://mirrors.example/metalink?repo=x`
///    has no usable extension and is caught there, at no extra cost, because the
///    probe had to happen anyway. See `download::run`.
///
/// Content sniffing is deliberately NOT applied to remote URLs at this point.
/// Doing so would mean fetching every URL twice â€?once to find out what it is,
/// once to download it â€?to answer a question the probe answers for free.
pub fn looks_like_document(arg: &str) -> bool {
    if metalink::is_metalink_filename(arg) {
        return true;
    }
    // A remote URL's content is settled at probe time, not here.
    if arg.contains("://") {
        return false;
    }
    file_looks_like_document(Path::new(arg))
}

/// Does this local file's content say it is a Metalink document?
///
/// A bounded read of the head, matched against [`metalink::looks_like_metalink`],
/// which requires both a `<metalink` element and one of the two namespace URIs.
/// The pair is what keeps this from firing on an unrelated XML file that happens
/// to mention the word â€?a false positive here means treating a user's actual
/// download as a mirror list.
pub fn file_looks_like_document(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 4096];
    match f.read(&mut head) {
        Ok(n) if n > 0 => metalink::looks_like_metalink(&head[..n]),
        _ => false,
    }
}

/// Read a document from a local path.
pub fn load_file(path: &Path) -> Result<Metalink, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read metalink {}: {e}", path.display()))?;
    metalink::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Fetch and read a document over HTTP.
///
/// Redirects are followed because mirror redirectors use them constantly â€?the
/// document lives behind a load balancer as often as not. The body is capped at
/// [`metalink::MAX_DOCUMENT`]: it is fetched before anything about it is known,
/// and an unbounded read of a body chosen by whoever answers is a
/// memory-exhaustion primitive no amount of care in the parser can fix.
pub async fn load_url(
    conn: &Arc<pdl_net::TlsCapableConnector>,
    url: &str,
    headers: &[String],
    agent: &str,
    max_redirs: u32,
) -> Result<Metalink, String> {
    let mut cur = crate::url::Url::parse(url).ok_or_else(|| format!("unparsable URL: {url}"))?;
    for _ in 0..=max_redirs {
        let px = crate::url::proxy_from_env();
        let t = cur
            .to_target(px.as_ref().map(|(h, p)| (h.as_str(), *p)))?
            .with_headers(headers.to_vec(), Some(agent.to_string()));
        // A HEAD first, only to learn whether this is a redirect. A GET that
        // lands on a 302 would have to be re-issued anyway, and this way the
        // capped body fetch happens exactly once against the final host.
        if let Ok(pr) = pdl_net::probe(conn.as_ref(), &t).await {
            if pr.is_redirect() {
                let loc = pr.location.clone().unwrap_or_default();
                match crate::url::Url::parse(&loc).or_else(|| cur.join(&loc)) {
                    Some(next) => {
                        cur = next;
                        continue;
                    }
                    None => return Err(format!("unusable redirect target {loc:?}")),
                }
            }
        }
        let body = pdl_net::fetch_small(conn.as_ref(), &t, FETCH_CAP)
            .await
            .map_err(|e| format!("cannot fetch metalink {url}: {e}"))?;
        let text = String::from_utf8(body)
            .map_err(|_| format!("{url}: the document is not valid UTF-8"))?;
        return metalink::parse(&text).map_err(|e| format!("{url}: {e}"));
    }
    Err(format!("too many redirects fetching {url}"))
}

/// Demotion applied to a mirror outside every preferred location.
///
/// Large enough that no publisher ranking can outrank the user's stated
/// geography, small enough to leave the demoted mirrors in their own relative
/// order â€?they are still reserves, and which reserve is drawn first should
/// still follow the publisher.
const OUTSIDE_PREFERRED_LOCATION: u32 = 1_000_000;

/// Demotion applied to a mirror outside the preferred protocol.
const OUTSIDE_PREFERRED_PROTOCOL: u32 = 1_000;

/// Order a file's mirrors into the one ranking that drives everything downstream.
///
/// # Why the result is a DENSE rank rather than the document's own numbers
///
/// Three different preferences have to end up in one field: the publisher's
/// ranking, the user's location list, and the user's protocol preference. Only
/// one number reaches [`pdl_core::plan::allocate`], and it decides both who gets
/// seated and how much share they get â€?so any preference not folded into it is
/// a preference that silently does nothing.
///
/// Folding them by arithmetic on the document's values does not work either: a
/// Metalink 3.0 `preference` maps into 1..101 while an unranked mirror sits at
/// 999999, so adding a penalty to one is a rounding error and to the other is a
/// no-op. Sorting first and then numbering 1, 2, 3... makes the field mean
/// exactly "final position", which is both what the allocator wants and what a
/// user reading `--verbose` output can check.
///
/// The share curve that follows from it (`1/rank`) is deliberately gentle. It is
/// a prior over an unmeasured quantity: the top mirror opens with more work, and
/// one repair corrects it if the ranking was wrong.
pub fn rank(file: &MetalinkFile, sel: &Selection) -> (Vec<MetaUrl>, Vec<String>) {
    let mut notes = Vec::new();
    let all = file.urls.len();
    let mut urls: Vec<MetaUrl> = file
        .urls
        .iter()
        .filter(|u| u.is_fetchable())
        .cloned()
        .collect();
    if urls.len() < all {
        let dropped: Vec<&str> = file
            .urls
            .iter()
            .filter(|u| !u.is_fetchable())
            .map(|u| u.kind.as_str())
            .collect();
        let mut kinds: Vec<&str> = dropped.clone();
        kinds.sort_unstable();
        kinds.dedup();
        notes.push(format!(
            "{} of {all} mirrors use a scheme this build cannot fetch ({}) and were dropped",
            all - urls.len(),
            kinds.join(", ")
        ));
    }

    let want_proto = sel
        .preferred_protocol
        .as_deref()
        .map(str::to_ascii_lowercase)
        .filter(|p| p != "none");

    // Sort key, most significant first: preferred location, preferred protocol,
    // the publisher's ranking, then the URL so the order is reproducible. A
    // download that opens different mirrors on every attempt cannot be debugged
    // from its logs.
    urls.sort_by(|a, b| {
        let key = |u: &MetaUrl| {
            let loc = match &u.location {
                Some(l) if !sel.locations.is_empty() => sel
                    .locations
                    .iter()
                    .position(|w| w == l)
                    .map(|i| i as u32)
                    .unwrap_or(OUTSIDE_PREFERRED_LOCATION),
                _ if sel.locations.is_empty() => 0,
                // A mirror with no stated location cannot be shown to be in the
                // user's preferred one, so it is demoted rather than assumed.
                _ => OUTSIDE_PREFERRED_LOCATION,
            };
            let proto = match &want_proto {
                Some(p) if u.kind.as_str() == p => 0,
                Some(_) => OUTSIDE_PREFERRED_PROTOCOL,
                // No stated preference: the TRANSPORT decides. HTTP(S) mirrors
                // can be spliced, repaired per chunk, and substituted; an FTP
                // source is a single sequential stream, and an FTP mirror the
                // publisher ranked first would hand the whole transfer to the
                // one scheme that turns all of that off. Real documents do
                // this â€?metalinker.org's own samples rank ftp:// at
                // preference 100 beside one http mirror. The publisher's
                // ranking still orders mirrors WITHIN each transport, and
                // `--metalink-preferred-protocol ftp` restores the old order
                // for a network where ftp is the better path.
                None => u.kind.transport_tier() as u32,
            };
            (loc, proto, u.priority, u.url.clone())
        };
        key(a).cmp(&key(b))
    });

    if want_proto.is_none() {
        let held_back = urls.iter().filter(|u| u.kind.transport_tier() > 0).count();
        if held_back > 0 && held_back < urls.len() {
            notes.push(format!(
                "{held_back} ftp mirror(s) held behind the http(s) ones â€?ftp streams from one \
                 connection, so it is the fallback; --metalink-preferred-protocol ftp to prefer it"
            ));
        }
    }

    if sel.unique_protocol {
        if let Some(first) = urls.first().map(|u| u.kind.clone()) {
            let before = urls.len();
            urls.retain(|u| u.kind == first);
            if urls.len() < before {
                notes.push(format!(
                    "--metalink-enable-unique-protocol: kept the {} mirrors, dropped {} on \
                     other protocols",
                    first.as_str(),
                    before - urls.len()
                ));
            }
        }
    }

    // Dense rank. See the doc note: this is the field every downstream decision
    // reads, so every preference has to be inside it.
    for (i, u) in urls.iter_mut().enumerate() {
        u.priority = (i + 1) as u32;
    }
    (urls, notes)
}

/// Does this file entry match the user's language/os/version filters?
///
/// An entry that says nothing about itself MATCHES. A document that omits `<os>`
/// is not claiming to be for a different platform, and filtering it out would
/// make `--metalink-os linux` select nothing from the many documents that do not
/// carry the field at all â€?which reads as "no such file" about a file that is
/// right there.
fn matches(f: &MetalinkFile, sel: &Selection) -> bool {
    let ok = |want: &Option<String>, have: &[String]| -> bool {
        match want {
            None => true,
            Some(_) if have.is_empty() => true,
            Some(w) => {
                let w = w.to_ascii_lowercase();
                have.contains(&w)
            }
        }
    };
    if !ok(&sel.language, &f.languages) || !ok(&sel.os, &f.oses) {
        return false;
    }
    match (&sel.version, &f.version) {
        (Some(w), Some(h)) => w.eq_ignore_ascii_case(h),
        (Some(_), None) => true,
        _ => true,
    }
}

/// Select and prepare every file this run should fetch.
///
/// Entries that cannot be used are dropped with a reason rather than silently:
/// an unsafe `name`, no fetchable mirror, no size. Returning an empty list is an
/// error, because "the document had files and none of them were usable" and "the
/// document had no files" are different problems with different fixes.
pub fn resolve(doc: &Metalink, sel: &Selection, from: &Origin) -> Result<Vec<Resolved>, String> {
    let mut chosen: Vec<&MetalinkFile> = match &sel.file {
        Some(want) => doc.file_named(want).map(|f| vec![f]).ok_or_else(|| {
            let have: Vec<&str> = doc.files.iter().map(|f| f.name.as_str()).collect();
            format!(
                "--metalink-file {want:?}: no such entry in {from} (it lists: {})",
                have.join(", ")
            )
        })?,
        None => doc.files.iter().collect(),
    };
    chosen.retain(|f| matches(f, sel));
    if chosen.is_empty() {
        return Err(format!(
            "{from}: no file entry matches the --metalink-language/--metalink-os/\
             --metalink-version filters"
        ));
    }

    let mut out = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    for f in chosen {
        let name = match f.safe_name() {
            Ok(n) => n.to_string(),
            Err(e) => {
                // A traversing name is the document trying to choose where bytes
                // land on this machine. Refuse it loudly: it is not a naming
                // quirk, and a document that contains one should not be trusted
                // for its other entries either.
                refused.push(format!("{e}"));
                continue;
            }
        };
        let (mut urls, mut notes) = rank(f, sel);
        if urls.is_empty() {
            refused.push(format!("{name}: no mirror this build can fetch from"));
            continue;
        }

        let attested = f.size.map(|size| {
            let pieces = match &f.pieces {
                None => None,
                Some(_) => match pdl_net::manifest::from_metalink(f) {
                    Ok(m) => {
                        notes.push(format!(
                            "per-chunk verification from the document: {} chunks of {}",
                            m.chunks.digests.len(),
                            crate::progress::human(m.object.chunk_size)
                        ));
                        Some(m)
                    }
                    Err(e) => {
                        // Worth a note rather than a failure: the whole-object
                        // digest still verifies the download, it just cannot
                        // localise a fault to one chunk.
                        notes.push(format!("<pieces> unusable, whole-file digest only: {e}"));
                        None
                    }
                },
            };
            Attested {
                size,
                digest: f.best_hash().map(|h| h.spec()),
                pieces,
                origin: from.to_string(),
            }
        });
        if attested.is_none() {
            notes.push(
                "the document states no <size> for this file, so mirrors cannot be checked \
                 against it and will be admitted on their own agreement instead"
                    .into(),
            );
        } else if attested.as_ref().is_some_and(|a| a.digest.is_none()) {
            notes.push("the document publishes no digest for this file".into());
        }

        // One TRANSPORT per job, decided by the best-ranked mirror.
        //
        // `rank` keeps every fetchable mirror so a report can show the whole
        // list, but a job cannot actually use a mixed one: the range engine
        // splices over HTTP, its probes and its reserve substitutions build
        // HTTP targets, and an `ftp://` entry in that list is a HEAD sent to
        // port 21 â€?a probe that cannot succeed and a "reserve" that could
        // never serve. Keeping only the leading tier makes the source list
        // mean what it says. An all-ftp document keeps its ftp mirrors and
        // takes the single-stream path, exactly as before.
        let lead_tier = urls[0].kind.transport_tier();
        let dropped_transport = urls
            .iter()
            .filter(|u| u.kind.transport_tier() != lead_tier)
            .count();
        if dropped_transport > 0 {
            notes.push(format!(
                "{dropped_transport} {} mirror(s) are not used for this transfer: byte ranges \
                 cannot be spliced across transports, so the {} mirrors carry it alone",
                urls.iter()
                    .find(|u| u.kind.transport_tier() != lead_tier)
                    .map(|u| u.kind.as_str())
                    .unwrap_or("other-transport"),
                urls[0].kind.as_str(),
            ));
            urls.retain(|u| u.kind.transport_tier() == lead_tier);
        }
        let plans = urls
            .iter()
            .map(|u| SourcePlan {
                priority: u.priority,
                max_connections: u.max_connections,
            })
            .collect();
        out.push(Resolved {
            name,
            urls: urls.iter().map(|u| u.url.clone()).collect(),
            plans,
            attested,
            notes,
            signed: f.signature.is_some(),
        });
    }

    if out.is_empty() {
        return Err(format!(
            "{from}: every file entry was refused ({})",
            refused.join("; ")
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdl_net::metalink::UrlKind;

    const DOC: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<metalink version="3.0" xmlns="http://www.metalinker.org/" generator="mirrormanager">
 <files>
  <file name="repomd.xml">
   <size>6285</size>
   <verification>
    <hash type="md5">8a9923bd9faba440fbe2c8ea5c5b301e</hash>
    <hash type="sha256">d201bd1eeb17086cd3aaf82b156810a5ba3f389e10b4472c9b2c7182f771a9ef</hash>
   </verification>
   <resources maxconnections="1">
    <url protocol="https" type="https" location="UA" preference="100">https://ua1.example/f</url>
    <url protocol="rsync" type="rsync" location="UA" preference="100">rsync://ua1.example/f</url>
    <url protocol="https" type="https" location="DE" preference="99">https://de1.example/f</url>
    <url protocol="http"  type="http"  location="US" preference="93">http://us1.example/f</url>
    <url protocol="https" type="https" location="US" preference="94">https://us2.example/f</url>
   </resources>
  </file>
 </files>
</metalink>"#;

    fn doc() -> Metalink {
        metalink::parse(DOC).unwrap()
    }

    fn origin() -> Origin {
        Origin::Url("https://mirrors.example/metalink?repo=x".into())
    }

    #[test]
    fn unfetchable_schemes_are_dropped_with_a_reason_the_user_can_read() {
        // Six of nineteen mirrors in the real Fedora document are rsync. Failing
        // to connect to each of them in turn is not a diagnosis.
        let d = doc();
        let (urls, notes) = rank(&d.files[0], &Selection::default());
        assert_eq!(urls.len(), 4);
        assert!(urls
            .iter()
            .all(|u| u.kind != UrlKind::Unsupported("rsync".into())));
        assert!(notes.iter().any(|n| n.contains("rsync")), "{notes:?}");
    }

    #[test]
    fn the_publishers_ranking_survives_as_a_dense_rank() {
        let d = doc();
        let (urls, _) = rank(&d.files[0], &Selection::default());
        assert_eq!(urls[0].url, "https://ua1.example/f", "preference=100 leads");
        // Dense: 1, 2, 3, 4 â€?the field the allocator reads means "final
        // position", not "whatever the document wrote".
        assert_eq!(
            urls.iter().map(|u| u.priority).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
    }

    #[test]
    fn a_location_preference_outranks_the_publishers_own_ordering() {
        // The user knows where they are; the publisher does not. `us` first must
        // beat `preference=100` on a mirror in Ukraine.
        let d = doc();
        let sel = Selection {
            locations: vec!["us".into(), "de".into()],
            ..Default::default()
        };
        let (urls, _) = rank(&d.files[0], &sel);
        assert!(urls[0].url.contains("us"), "{:?}", urls[0].url);
        assert!(urls[1].url.contains("us"), "{:?}", urls[1].url);
        assert!(urls[2].url.contains("de1"), "{:?}", urls[2].url);
        // Within a location, the publisher's ranking still decides: preference
        // 94 (https) beats 93 (http).
        assert_eq!(urls[0].url, "https://us2.example/f");
        // The demoted mirrors are still present â€?they are reserves, not
        // rejects.
        assert_eq!(urls.len(), 4);
    }

    #[test]
    fn a_protocol_preference_reorders_and_unique_protocol_filters() {
        let d = doc();
        let sel = Selection {
            preferred_protocol: Some("http".into()),
            ..Default::default()
        };
        let (urls, _) = rank(&d.files[0], &sel);
        assert_eq!(urls[0].url, "http://us1.example/f");
        assert_eq!(urls.len(), 4, "preference alone does not drop anything");

        let sel = Selection {
            preferred_protocol: Some("http".into()),
            unique_protocol: true,
            ..Default::default()
        };
        let (urls, notes) = rank(&d.files[0], &sel);
        assert_eq!(urls.len(), 1, "only the http mirror survives: {urls:?}");
        assert!(
            notes.iter().any(|n| n.contains("unique-protocol")),
            "{notes:?}"
        );

        // `none` means the user explicitly does not care.
        let sel = Selection {
            preferred_protocol: Some("none".into()),
            ..Default::default()
        };
        let (urls, _) = rank(&d.files[0], &sel);
        assert_eq!(urls[0].url, "https://ua1.example/f");
    }

    #[test]
    fn ranking_is_reproducible() {
        // A download that opens different mirrors on every attempt cannot be
        // debugged from its logs.
        let d = doc();
        let sel = Selection {
            locations: vec!["us".into()],
            ..Default::default()
        };
        let first = rank(&d.files[0], &sel).0;
        for _ in 0..20 {
            assert_eq!(rank(&d.files[0], &sel).0, first);
        }
    }

    #[test]
    fn resolution_carries_the_size_digest_and_per_mirror_ceiling() {
        let d = doc();
        let r = resolve(&d, &Selection::default(), &origin()).unwrap();
        assert_eq!(r.len(), 1);
        let f = &r[0];
        assert_eq!(f.name, "repomd.xml");
        assert_eq!(f.urls.len(), 4);
        let a = f.attested.as_ref().unwrap();
        assert_eq!(a.size, 6285);
        // The strongest published digest, not the first one listed.
        assert!(a.digest.as_deref().unwrap().starts_with("sha256:"));
        assert!(a.origin.contains("mirrors.example"));
        // `<resources maxconnections="1">` is the DEFAULT for each mirror, not a
        // cap on the file: read as an aggregate it would open one connection in
        // total across nineteen mirrors and make the list worthless. Applied in
        // the parser, so every plan carries it.
        assert!(f.plans.iter().all(|p| p.max_connections == Some(1)));
        assert_eq!(
            f.plans.iter().map(|p| p.priority).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert!(!f.signed);
    }

    #[test]
    fn ftp_mirrors_wait_behind_http_unless_the_user_prefers_them() {
        // The catix sample on metalinker.org: three ftp mirrors at
        // preference=100 beside one http. The publisher is ranking HOSTS; the
        // engine routes on the first URL, and an ftp lead does not merely go
        // first â€?it drops the transfer to a single sequential stream with no
        // splicing, no chunk repair and no reserves. Transport is therefore
        // the major key and the publisher orders mirrors within it.
        let src = r#"<metalink version="3.0" xmlns="http://www.metalinker.org/"><files>
          <file name="c.iso"><size>10</size><resources>
            <url type="ftp" preference="100">ftp://a.example/c.iso</url>
            <url type="ftp" preference="90">ftp://b.example/c.iso</url>
            <url type="http" preference="10">http://h.example/c.iso</url>
          </resources></file></files></metalink>"#;
        let doc = metalink::parse(src).unwrap();
        let (urls, notes) = rank(&doc.files[0], &Selection::default());
        assert_eq!(
            urls[0].url, "http://h.example/c.iso",
            "http leads: {urls:?}"
        );
        // The publisher's own order survives WITHIN the ftp tier.
        assert_eq!(urls[1].url, "ftp://a.example/c.iso");
        assert_eq!(urls[2].url, "ftp://b.example/c.iso");
        assert!(
            notes.iter().any(|n| n.contains("held behind the http(s)")),
            "the demotion is said, not silent: {notes:?}"
        );

        // The user outranks the default: a network where ftp is the better
        // path can say so, and the old order comes back.
        let sel = Selection {
            preferred_protocol: Some("ftp".into()),
            ..Selection::default()
        };
        let (urls, _) = rank(&doc.files[0], &sel);
        assert_eq!(urls[0].url, "ftp://a.example/c.iso");

        // All-ftp is not demoted below anything: there is nothing to wait
        // behind, and the single-stream path is genuinely the right one.
        let only = r#"<metalink version="3.0" xmlns="http://www.metalinker.org/"><files>
          <file name="c.iso"><size>10</size><resources>
            <url type="ftp" preference="100">ftp://a.example/c.iso</url>
          </resources></file></files></metalink>"#;
        let doc = metalink::parse(only).unwrap();
        let (urls, notes) = rank(&doc.files[0], &Selection::default());
        assert_eq!(urls[0].url, "ftp://a.example/c.iso");
        assert!(
            !notes.iter().any(|n| n.contains("held behind")),
            "nothing was held back, so nothing is claimed: {notes:?}"
        );
    }

    #[test]
    fn a_mixed_transport_list_resolves_to_one_transport_with_the_reason_stated() {
        // `rank` keeps everything so a report can show the whole list; the JOB
        // cannot use a mixed one â€?the range engine probes and substitutes over
        // HTTP targets, so an ftp entry would be a request sent to port 21.
        let src = r#"<metalink version="3.0" xmlns="http://www.metalinker.org/"><files>
          <file name="c.iso"><size>10</size><resources>
            <url type="ftp" preference="100">ftp://a.example/c.iso</url>
            <url type="http" preference="10">http://h.example/c.iso</url>
            <url type="https" preference="5">https://s.example/c.iso</url>
          </resources></file></files></metalink>"#;
        let doc = metalink::parse(src).unwrap();
        let r = resolve(&doc, &Selection::default(), &origin()).unwrap();
        assert_eq!(
            r[0].urls,
            vec!["http://h.example/c.iso", "https://s.example/c.iso"],
            "http and https splice together; ftp does not ride along"
        );
        assert_eq!(r[0].plans.len(), r[0].urls.len(), "plans stay aligned");
        assert!(
            r[0].notes
                .iter()
                .any(|n| n.contains("cannot be spliced across transports")),
            "the drop is said, not silent: {:?}",
            r[0].notes
        );

        // All-ftp keeps its mirrors and takes the single-stream path.
        let only = r#"<metalink version="3.0" xmlns="http://www.metalinker.org/"><files>
          <file name="c.iso"><size>10</size><resources>
            <url type="ftp">ftp://a.example/c.iso</url>
            <url type="ftp">ftp://b.example/c.iso</url>
          </resources></file></files></metalink>"#;
        let doc = metalink::parse(only).unwrap();
        let r = resolve(&doc, &Selection::default(), &origin()).unwrap();
        assert_eq!(r[0].urls.len(), 2);
        assert!(r[0].urls.iter().all(|u| u.starts_with("ftp://")));
    }

    #[test]
    fn a_traversing_name_is_refused_rather_than_written() {
        // The document choosing where bytes land on this machine.
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">
            <file name="../../.ssh/authorized_keys"><size>1</size>
            <url>https://h/x</url></file></metalink>"#;
        let d = metalink::parse(src).unwrap();
        let e = resolve(&d, &Selection::default(), &origin()).unwrap_err();
        assert!(e.contains("unsafe file name"), "{e}");
    }

    #[test]
    fn a_file_with_only_unfetchable_mirrors_is_refused_with_that_reason() {
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">
            <file name="f"><size>1</size>
            <url>rsync://h/x</url><url>ftps://h/x</url></file></metalink>"#;
        let d = metalink::parse(src).unwrap();
        let e = resolve(&d, &Selection::default(), &origin()).unwrap_err();
        assert!(e.contains("no mirror this build can fetch from"), "{e}");
    }

    #[test]
    fn selecting_a_named_file_and_failing_to_lists_what_is_there() {
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">
            <file name="a.bin"><size>1</size><url>https://h/a</url></file>
            <file name="b.bin"><size>1</size><url>https://h/b</url></file>
          </metalink>"#;
        let d = metalink::parse(src).unwrap();
        let r = resolve(
            &d,
            &Selection {
                file: Some("b.bin".into()),
                ..Default::default()
            },
            &origin(),
        )
        .unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "b.bin");

        let e = resolve(
            &d,
            &Selection {
                file: Some("c.bin".into()),
                ..Default::default()
            },
            &origin(),
        )
        .unwrap_err();
        assert!(e.contains("a.bin") && e.contains("b.bin"), "{e}");
    }

    #[test]
    fn an_entry_that_says_nothing_about_its_platform_still_matches() {
        // Filtering it out would make `--metalink-os linux` select nothing from
        // the many documents that omit the field â€?which reads as "no such file"
        // about a file that is right there.
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">
            <file name="quiet.bin"><size>1</size><url>https://h/a</url></file>
            <file name="mac.bin"><size>1</size><os>darwin</os><url>https://h/b</url></file>
            <file name="lin.bin"><size>1</size><os>linux</os><url>https://h/c</url></file>
          </metalink>"#;
        let d = metalink::parse(src).unwrap();
        let sel = Selection {
            os: Some("linux".into()),
            ..Default::default()
        };
        let got: Vec<String> = resolve(&d, &sel, &origin())
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(got, vec!["quiet.bin", "lin.bin"]);
    }

    #[test]
    fn pieces_become_a_manifest_and_a_mismatched_grid_becomes_a_note() {
        let good = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="f">
            <size>8</size>
            <pieces length="4" type="sha-1">
              <hash>1111111111111111111111111111111111111111</hash>
              <hash>2222222222222222222222222222222222222222</hash>
            </pieces><url>https://h/f</url></file></metalink>"#;
        let d = metalink::parse(good).unwrap();
        let r = resolve(&d, &Selection::default(), &origin()).unwrap();
        let m = r[0].attested.as_ref().unwrap().pieces.as_ref().unwrap();
        assert_eq!(m.chunks.digests.len(), 2);
        assert!(
            r[0].notes.iter().any(|n| n.contains("per-chunk")),
            "{:?}",
            r[0].notes
        );

        // A grid that does not tile is a document defect. The whole-file digest
        // still verifies the download, so this is a note, not a failure.
        let bad = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="f">
            <size>99</size>
            <pieces length="4" type="sha-1">
              <hash>1111111111111111111111111111111111111111</hash>
            </pieces><url>https://h/f</url></file></metalink>"#;
        let d = metalink::parse(bad).unwrap();
        let r = resolve(&d, &Selection::default(), &origin()).unwrap();
        assert!(r[0].attested.as_ref().unwrap().pieces.is_none());
        assert!(
            r[0].notes.iter().any(|n| n.contains("<pieces> unusable")),
            "{:?}",
            r[0].notes
        );
    }

    #[test]
    fn a_signature_is_reported_as_unverified_rather_than_ignored() {
        // Silence is wrong in both directions: no mention reads as "nothing to
        // check", and a bare mention reads as "checked".
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="f">
            <size>1</size><url>https://h/f</url>
            <signature mediatype="application/pgp-signature">-----BEGIN-----
            x
            -----END-----</signature></file></metalink>"#;
        let d = metalink::parse(src).unwrap();
        let r = resolve(&d, &Selection::default(), &origin()).unwrap();
        assert!(r[0].signed);
        assert!(r[0].signature_note().unwrap().contains("does not verify"));
    }

    #[test]
    fn a_document_is_recognised_by_name_or_by_content() {
        assert!(looks_like_document("f.meta4"));
        assert!(looks_like_document("fedora.metalink"));
        // A remote URL is judged on its name here and on its `Content-Type` at
        // probe time. Sniffing content here would mean fetching every URL twice
        // to answer a question the probe answers for free.
        assert!(!looks_like_document(
            "https://mirrors.fedoraproject.org/metalink?repo=fedora-40&arch=x86_64"
        ));
        assert!(!looks_like_document("https://h/big.iso"));

        // A LOCAL file is read, because a document saved by a browser is as
        // likely to be called `metalink.xml` as anything else.
        let dir = std::env::temp_dir();
        let doc = dir.join(format!("playdl_sniff_{}.xml", std::process::id()));
        std::fs::write(&doc, DOC).unwrap();
        assert!(looks_like_document(doc.to_str().unwrap()));

        // And an unrelated XML file is not one. The namespace requirement is
        // what keeps a false positive from turning a user's download into a
        // mirror list.
        let other = dir.join(format!("playdl_sniff_{}_other.xml", std::process::id()));
        std::fs::write(
            &other,
            "<?xml version=\"1.0\"?><notes><metalink-ish/></notes>",
        )
        .unwrap();
        assert!(!looks_like_document(other.to_str().unwrap()));

        assert!(!looks_like_document("/nonexistent/path/to/nothing"));
        let _ = std::fs::remove_file(&doc);
        let _ = std::fs::remove_file(&other);
    }
}
