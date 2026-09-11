// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Metalink documents across the C ABI.
//!
//! # What an embedder gets that a URL list does not give them
//!
//! `hydra_job_create` already takes several URLs, so it would be reasonable to
//! ask why a mirror list needs its own entry point at all. The answer is that
//! the URLs are the least valuable thing in the document. Handing the engine a
//! bare list of mirrors leaves it with no size it can trust, no digest, no
//! ranking, and — decisively — no way to admit a second source: mirror assembly
//! is gated on every source agreeing about a strong validator, and independent
//! mirror operators running independent web servers cannot share an `ETag`. A
//! nineteen-mirror list passed as nineteen URLs downloads from exactly one of
//! them.
//!
//! A document changes that, because it states the facts from OUTSIDE the
//! mirrors:
//!
//! * **a size**, which admits a mirror without a validator handshake;
//! * **a digest**, which is what actually catches a mirror serving a stale
//!   build, and which is wired into the same post-transfer verification
//!   `hydra_job_config_t.checksum` already drives;
//! * **`<pieces>`**, which turns a corrupt download into a corrupt CHUNK,
//!   refetched from a different mirror;
//! * **a ranking**, which decides the first split and the order of the reserve
//!   bench.
//!
//! # Two layers, deliberately
//!
//! [`hydra_metalink_files`](crate::exports::hydra_metalink_files) and
//! [`hydra_metalink_mirrors`](crate::exports::hydra_metalink_mirrors) let a host
//! application show the user what a document offers before anything is fetched —
//! which matters most on mobile, where a 4 GB image on a metered link is a
//! decision and not a detail.
//! [`hydra_job_create_from_metalink`](crate::exports::hydra_job_create_from_metalink)
//! is the one call that does the whole thing.
//!
//! The parsing and the choosing both live in `hya-net`/`hya-core`; this module
//! is the boundary crossing and nothing else.

use crate::abi::hydra_error_code_t as E;
use crate::err::Detail;
use pdl_core::SourcePlan;
use pdl_net::metalink::{MetalinkFile, NO_PRIORITY};

/// Largest document fetched over the network.
///
/// The document is fetched before anything about it is known, so an unbounded
/// read of a body chosen by whoever answers is a memory-exhaustion primitive no
/// amount of care in the parser can fix.
pub(crate) const FETCH_CAP: usize = pdl_net::metalink::MAX_DOCUMENT;

/// A parsed document, behind the opaque handle the C side holds.
pub(crate) struct Doc {
    pub inner: pdl_net::Metalink,
    /// Where it came from, for the `attested by` line in a job's log.
    pub origin: String,
}

/// Everything one entry contributes to a job.
#[derive(Debug)]
pub(crate) struct Chosen {
    pub name: String,
    pub urls: Vec<String>,
    pub plans: Vec<SourcePlan>,
    pub size: Option<u64>,
    /// `algorithm:hex`, the strongest digest the document published.
    pub digest: Option<String>,
    pub pieces: Option<pdl_net::manifest::Manifest>,
}

fn invalid(msg: impl Into<String>) -> Detail {
    Detail {
        code: E::HYDRA_ERR_INVALID_ARGUMENT as u32,
        message: msg.into(),
        ..Default::default()
    }
}

/// Read a document from bytes already in hand.
pub(crate) fn parse(text: &str, origin: &str) -> Result<Doc, Detail> {
    if text.len() > FETCH_CAP {
        return Err(invalid(format!(
            "the document is {} bytes; at most {FETCH_CAP} are read",
            text.len()
        )));
    }
    pdl_net::metalink::parse(text)
        .map(|inner| Doc {
            inner,
            origin: origin.to_string(),
        })
        .map_err(|e| invalid(format!("{origin}: {e}")))
}

/// Read a document from a local path.
pub(crate) fn open(path: &str) -> Result<Doc, Detail> {
    let text = std::fs::read_to_string(path).map_err(|e| crate::err::from_io(&e))?;
    parse(&text, path)
}

/// Fetch a document over HTTP and read it.
///
/// Redirects are followed because mirror redirectors use them constantly — the
/// document lives behind a load balancer as often as not.
pub(crate) async fn fetch(
    conn: &std::sync::Arc<pdl_net::TlsCapableConnector>,
    url: &str,
    headers: &[String],
    agent: &str,
    max_redirs: u32,
) -> Result<Doc, Detail> {
    let net = |m: String| Detail {
        code: E::HYDRA_ERR_NETWORK as u32,
        message: m,
        ..Default::default()
    };
    let mut cur = crate::url::Url::parse(url).map_err(|e| Detail {
        code: E::HYDRA_ERR_INVALID_URL as u32,
        message: e,
        ..Default::default()
    })?;
    for _ in 0..=max_redirs {
        // Direct, and without the job's proxy or credentials: this fetch is not
        // the transfer, it is the client asking a publisher's own host what the
        // object is. Routing it through a job's proxy would attach one job's
        // configuration to a document that may describe several.
        let t = if cur.tls() {
            pdl_net::Target::direct_tls(&cur.host, cur.port, &cur.path)
        } else {
            pdl_net::Target::direct(&cur.host, cur.port, &cur.path)
        }
        .with_headers(headers.to_vec(), Some(agent.to_string()));
        // A HEAD first, only to learn whether this is a redirect. A GET that
        // lands on a 302 would have to be re-issued anyway, and this way the
        // capped body fetch happens exactly once against the final host.
        if let Ok(pr) = pdl_net::probe(conn.as_ref(), &t).await {
            if pr.is_redirect() {
                let loc = pr.location.clone().unwrap_or_default();
                match cur.join(&loc) {
                    Ok(next) => {
                        cur = next;
                        continue;
                    }
                    Err(e) => return Err(net(format!("unusable redirect target {loc:?}: {e}"))),
                }
            }
        }
        let body = pdl_net::fetch_small(conn.as_ref(), &t, FETCH_CAP)
            .await
            .map_err(|e| net(format!("cannot fetch metalink {url}: {e}")))?;
        let text = String::from_utf8(body)
            .map_err(|_| invalid(format!("{url}: the document is not valid UTF-8")))?;
        return parse(&text, url);
    }
    Err(net(format!("too many redirects fetching {url}")))
}

/// The mirrors of one entry, best-first, as a dense rank.
///
/// Dense rather than the document's own numbers because one number reaches
/// [`pdl_core::plan::allocate`] and it decides both who is seated and how much
/// share they get. A Metalink 3.0 `preference` maps into 1..101 while an
/// unranked mirror sits at [`NO_PRIORITY`], so arithmetic on the document's
/// values cannot express "this one first" consistently across dialects.
/// Numbering the sorted list makes the field mean exactly "final position".
///
/// Unfetchable schemes (`rsync://` and friends) are dropped here rather than
/// earlier: a reader should be able to SEE the whole list, and only a source
/// list has to exclude them.
pub(crate) fn ranked(f: &MetalinkFile) -> Vec<(String, SourcePlan, Option<String>, String, u32)> {
    let mut urls: Vec<&pdl_net::MetaUrl> = f.fetchable_urls();
    // Transport first, the publisher's ranking within it: HTTP(S) mirrors can
    // be spliced and repaired per chunk, an FTP source is a single sequential
    // stream, and real documents rank ftp:// first (metalinker.org's own
    // samples do). See `UrlKind::transport_tier`.
    urls.sort_by_key(|u| (u.kind.transport_tier(), u.priority, u.url.clone()));
    urls.iter()
        .enumerate()
        .map(|(i, u)| {
            (
                u.url.clone(),
                SourcePlan {
                    priority: (i + 1) as u32,
                    max_connections: u.max_connections,
                },
                u.location.clone(),
                u.kind.as_str().to_string(),
                u.priority,
            )
        })
        .collect()
}

/// Turn one entry into the sources, ranking and attestation a job runs on.
///
/// Refuses rather than approximates in the two cases where continuing would
/// produce a wrong file quietly: a name that escapes the output directory, and
/// an entry with no mirror this build can fetch from.
pub(crate) fn choose(doc: &Doc, index: usize) -> Result<Chosen, Detail> {
    let f = doc.inner.files.get(index).ok_or_else(|| {
        invalid(format!(
            "file index {index} is past the end of the document"
        ))
    })?;
    let name = f
        .safe_name()
        .map_err(|e| invalid(format!("{}: {}", doc.origin, e.why)))?
        .to_string();
    let mut ranked = ranked(f);
    if ranked.is_empty() {
        return Err(invalid(format!(
            "{}: {:?} lists {} mirror(s), none on a scheme this build can fetch",
            doc.origin,
            name,
            f.urls.len()
        )));
    }
    // One TRANSPORT per job. `ranked` keeps every fetchable mirror so
    // `hydra_metalink_mirrors` can show the whole list, but the engine splices
    // over HTTP and builds HTTP targets for its probes and reserves — an
    // `ftp://` entry in a mixed job is a request sent to port 21. The leading
    // tier carries the transfer; an all-ftp entry keeps its ftp mirrors and
    // takes the single-stream path.
    let lead = tier_of(&ranked[0].3);
    ranked.retain(|(.., proto, _)| tier_of(proto) == lead);
    // A piece list that does not tile the stated size describes a different
    // object; applying it anyway would report every chunk as corrupt. Dropped
    // with the whole-file digest still in place rather than failing the job,
    // because the object itself is still perfectly fetchable.
    let pieces = pdl_net::manifest::from_metalink(f).ok();
    Ok(Chosen {
        name,
        urls: ranked.iter().map(|(u, ..)| u.clone()).collect(),
        plans: ranked.iter().map(|(_, p, ..)| *p).collect(),
        size: f.size,
        digest: f.best_hash().map(|h| h.spec()),
        pieces,
    })
}

/// The transport tier of a scheme string, as `ranked` reports it.
///
/// A thin bridge: `ranked` hands its consumers the scheme as a string, and the
/// tier logic lives on [`pdl_net::metalink::UrlKind`] where every frontend
/// shares it.
fn tier_of(proto: &str) -> u8 {
    pdl_net::metalink::UrlKind::from_url(&format!("{proto}://x")).transport_tier()
}

/// Index of the entry whose name matches `want`, by full name or base name.
///
/// Both spellings, because a user picks what a listing showed them and a listing
/// generally shows the base name.
pub(crate) fn index_of(doc: &Doc, want: &str) -> Option<usize> {
    doc.inner
        .files
        .iter()
        .position(|f| f.name == want || f.base_name() == Some(want))
}

/// The digest spec a document published, as the ABI's checksum pair.
///
/// `None` when the algorithm is one this build does not compare (a CRC), which
/// is reported as "not checked" rather than as a pass — a verification that
/// means nothing is worse than an honest absence.
pub(crate) fn checksum_of(spec: &str) -> Option<(crate::engine::Algo, Vec<u8>)> {
    use crate::engine::Algo;
    let (a, hex) = spec.split_once(':')?;
    let algo = match pdl_net::digest::Algo::parse(a)? {
        pdl_net::digest::Algo::Md5 => Algo::Md5,
        pdl_net::digest::Algo::Sha1 => Algo::Sha1,
        pdl_net::digest::Algo::Sha256 => Algo::Sha256,
        pdl_net::digest::Algo::Sha512 => Algo::Sha512,
        pdl_net::digest::Algo::Crc32 | pdl_net::digest::Algo::Crc32c => return None,
    };
    // The parser already refuses malformed document digests, so this cannot
    // fire from a real `Attested` — but this function is the boundary the
    // verifier's `want` comes through, and a truncated digest that slipped in
    // any other way would report a GOOD file as a checksum failure. "Not
    // checked" is the honest answer for a spec that cannot be checked.
    if hex.len() != algo.len() * 2 {
        return None;
    }
    let bytes = (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .ok()?;
    Some((algo, bytes))
}

/// The dialect, as the ABI enumerator.
pub(crate) fn version_of(doc: &Doc) -> crate::abi::hydra_metalink_version_t {
    use crate::abi::hydra_metalink_version_t as V;
    match doc.inner.version {
        Some(pdl_net::metalink::Version::V3) => V::HYDRA_METALINK_V3,
        Some(pdl_net::metalink::Version::V4) => V::HYDRA_METALINK_V4,
        None => V::HYDRA_METALINK_UNKNOWN,
    }
}

/// An unranked plan, for a source list that came from somewhere else.
pub(crate) fn unranked(n: usize) -> Vec<SourcePlan> {
    vec![
        SourcePlan {
            priority: NO_PRIORITY,
            max_connections: None,
        };
        n
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">
      <file name="big.iso">
        <size>4194304</size>
        <hash type="md5">0123456789abcdef0123456789abcdef</hash>
        <hash type="sha-256">d201bd1eeb17086cd3aaf82b156810a5ba3f389e10b4472c9b2c7182f771a9ef</hash>
        <pieces length="1048576" type="sha-256">
          <hash>0000000000000000000000000000000000000000000000000000000000000001</hash>
          <hash>0000000000000000000000000000000000000000000000000000000000000002</hash>
          <hash>0000000000000000000000000000000000000000000000000000000000000003</hash>
          <hash>0000000000000000000000000000000000000000000000000000000000000004</hash>
        </pieces>
        <url priority="9">https://slow.example/big.iso</url>
        <url priority="1">https://fast.example/big.iso</url>
        <url>rsync://rs.example/big.iso</url>
      </file>
    </metalink>"#;

    #[test]
    fn an_entry_becomes_a_ranked_source_list_with_its_attestation() {
        let doc = parse(DOC, "test").unwrap();
        let c = choose(&doc, 0).unwrap();
        assert_eq!(c.name, "big.iso");
        // Best mirror first, dense ranks, and the rsync mirror is not a source.
        assert_eq!(
            c.urls,
            vec![
                "https://fast.example/big.iso",
                "https://slow.example/big.iso"
            ]
        );
        assert_eq!(c.plans[0].priority, 1);
        assert_eq!(c.plans[1].priority, 2);
        assert_eq!(c.size, Some(4_194_304));
        // The STRONGEST digest, not the first one listed: verifying against the
        // md5 with a sha256 in hand is a choice, and it is the wrong one.
        assert_eq!(
            c.digest.as_deref(),
            Some("sha256:d201bd1eeb17086cd3aaf82b156810a5ba3f389e10b4472c9b2c7182f771a9ef")
        );
        let m = c.pieces.expect("pieces tile a 4 MiB object at 1 MiB");
        assert_eq!(m.chunks.digests.len(), 4);
        assert_eq!(m.object.chunk_size, 1 << 20);
    }

    #[test]
    fn the_document_digest_crosses_the_abi_as_the_checksum_the_engine_already_verifies() {
        let (algo, bytes) =
            checksum_of("sha256:d201bd1eeb17086cd3aaf82b156810a5ba3f389e10b4472c9b2c7182f771a9ef")
                .unwrap();
        assert_eq!(algo, crate::engine::Algo::Sha256);
        assert_eq!(bytes.len(), 32);
        assert_eq!(bytes[0], 0xd2);
        // A CRC is not a digest a verification result may be reported from.
        assert!(checksum_of("crc32:deadbeef").is_none());
        assert!(checksum_of("not-a-spec").is_none());
        // A digest of the wrong width cannot be checked, and passing a
        // truncated `want` to the verifier would fail a GOOD file. Both the
        // odd-length and the wrong-but-even-length spellings must be refused.
        assert!(checksum_of(&format!("sha256:{}", "a".repeat(63))).is_none());
        assert!(checksum_of(&format!("sha256:{}", "a".repeat(62))).is_none());
        assert!(checksum_of(&format!("md5:{}", "a".repeat(64))).is_none());
    }

    #[test]
    fn a_job_takes_one_transport_while_the_mirror_listing_shows_them_all() {
        // `hydra_metalink_mirrors` is the display path and keeps everything a
        // caller could show; `choose` is the job path, and the engine it feeds
        // probes and substitutes over HTTP targets — an ftp entry there is a
        // request sent to port 21.
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="f">
            <size>4</size>
            <url priority="1">ftp://a.example/f</url>
            <url priority="2">https://h.example/f</url>
          </file></metalink>"#;
        let doc = parse(src, "test").unwrap();
        assert_eq!(
            ranked(&doc.inner.files[0]).len(),
            2,
            "the listing shows both"
        );
        let c = choose(&doc, 0).unwrap();
        assert_eq!(
            c.urls,
            vec!["https://h.example/f"],
            "the job carries the transport it can splice"
        );
        assert_eq!(c.plans.len(), 1);
    }

    #[test]
    fn an_entry_with_no_fetchable_mirror_is_refused_with_the_count() {
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="f">
            <size>1</size><url>rsync://a/f</url><url>rsync://b/f</url></file></metalink>"#;
        let doc = parse(src, "test").unwrap();
        let e = choose(&doc, 0).unwrap_err();
        assert!(e.message.contains("2 mirror(s)"), "{}", e.message);
        assert!(e.message.contains("none on a scheme"), "{}", e.message);
    }

    #[test]
    fn a_name_that_escapes_the_output_directory_is_refused_before_anything_is_fetched() {
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">
            <file name="../../etc/cron.d/x"><size>1</size>
            <url>https://a/f</url></file></metalink>"#;
        let doc = parse(src, "test").unwrap();
        assert!(choose(&doc, 0).is_err());
    }

    #[test]
    fn pieces_that_do_not_tile_the_size_are_dropped_and_the_job_still_runs() {
        // The document contradicts itself. Failing the job would be wrong — the
        // object is perfectly fetchable — and applying the pieces anyway would
        // report every chunk as corrupt.
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="f">
            <size>100</size>
            <pieces length="4" type="sha-1">
              <hash>1111111111111111111111111111111111111111</hash>
            </pieces>
            <url>https://a/f</url></file></metalink>"#;
        let doc = parse(src, "test").unwrap();
        let c = choose(&doc, 0).unwrap();
        assert!(c.pieces.is_none());
        assert_eq!(c.size, Some(100));
    }

    #[test]
    fn an_index_past_the_end_says_so_rather_than_taking_the_first_entry() {
        let doc = parse(DOC, "test").unwrap();
        let e = choose(&doc, 7).unwrap_err();
        assert!(e.message.contains("past the end"), "{}", e.message);
        assert_eq!(index_of(&doc, "big.iso"), Some(0));
        assert_eq!(index_of(&doc, "nope"), None);
    }
}
