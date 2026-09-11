// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A pull parser for the XML subset Metalink documents are written in.
//!
//! # Why this is not a dependency
//!
//! The same reason `http.rs` is a hand-rolled HTTP/1.1 client and `polite.rs`
//! parses its own IMF-fixdate: this crate is meant to stay embeddable, and the
//! whole of what it needs from an XML library is "walk elements, read
//! attributes, read text". A general parser brings DTD processing, entity
//! expansion, XPath-adjacent APIs, and a namespace engine — every one of which
//! is surface this crate does not use and would still have to trust with
//! untrusted input from a mirror list.
//!
//! The subset implemented here is exactly what RFC 5854 §4 and the Metalink 3.0
//! schema require: elements, attributes, character data, comments, processing
//! instructions, CDATA sections, and the five predefined entities plus numeric
//! character references.
//!
//! # What is deliberately refused
//!
//! **Internal DTD subsets and custom entity declarations.** A `<!DOCTYPE>` with
//! an internal subset is skipped without processing its declarations, and a
//! reference to any entity other than the five predefined ones is an error
//! rather than an empty expansion. That closes the "billion laughs" class
//! outright instead of bounding it: there is no expansion step to bound, and a
//! document that needs one is not a Metalink document.
//!
//! **Unbounded nesting.** Depth is capped ([`MAX_DEPTH`]). Metalink's deepest
//! legal path is five elements; a document nested past the cap is hostile or
//! broken, and either way is not worth the stack.
//!
//! # Namespaces
//!
//! Prefixes are split off and reported, but no prefix-to-URI resolution is
//! performed. Callers match on the LOCAL name, which is what makes one parser
//! read both `<metalink xmlns="urn:ietf:params:xml:ns:metalink">` and a
//! document that writes `<mm0:timestamp>` in a foreign namespace: the local name
//! is the discriminator in both, and the document's own root namespace is
//! checked once by [`crate::metalink`] to decide which dialect it is.

use std::fmt;

/// Maximum element nesting. Metalink's deepest legal path is
/// `metalink/files/file/resources/url` — five.
pub const MAX_DEPTH: usize = 64;

/// Largest attribute count on one element before the document is refused.
const MAX_ATTRS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attr {
    /// Local name, with any namespace prefix removed.
    pub name: String,
    /// Namespace prefix, when the attribute was written as `pfx:name`.
    pub prefix: Option<String>,
    pub value: String,
}

/// One step of the document.
///
/// A self-closing element (`<url/>`) produces [`Event::Start`] immediately
/// followed by [`Event::End`], so a caller never has to special-case it. That is
/// the single most common source of bugs in hand-written XML consumers and it
/// costs one boolean here to remove.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Start {
        name: String,
        prefix: Option<String>,
        attrs: Vec<Attr>,
    },
    End {
        name: String,
        prefix: Option<String>,
    },
    /// Character data, with entities already resolved.
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmlError {
    pub at: usize,
    pub why: String,
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at byte {})", self.why, self.at)
    }
}

impl std::error::Error for XmlError {}

/// A pull parser over a `&str`.
pub struct Reader<'a> {
    src: &'a [u8],
    pos: usize,
    /// Pending `End` for a self-closing element.
    pending_end: Option<(String, Option<String>)>,
    depth: usize,
    /// Open element names, so a mismatched close tag is reported rather than
    /// silently accepted. A mismatched tag in a mirror list means the document
    /// is not the document it claims to be, and continuing would build a source
    /// list out of whatever the truncation happened to leave behind.
    open: Vec<String>,
}

impl<'a> Reader<'a> {
    pub fn new(src: &'a str) -> Self {
        // A UTF-8 BOM is legal at the head of an XML document and is not part of
        // the prolog, so it has to be stepped over before `<?xml` is looked for.
        let b = src.as_bytes();
        let pos = usize::from(b.starts_with(&[0xEF, 0xBB, 0xBF])) * 3;
        Reader {
            src: b,
            pos,
            pending_end: None,
            depth: 0,
            open: Vec::new(),
        }
    }

    fn err<T>(&self, why: impl Into<String>) -> Result<T, XmlError> {
        Err(XmlError {
            at: self.pos,
            why: why.into(),
        })
    }

    #[inline]
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn starts_with(&self, s: &[u8]) -> bool {
        self.src[self.pos..].starts_with(s)
    }

    /// Advance past `needle`, returning what lay before it.
    fn take_until(&mut self, needle: &[u8]) -> Result<&'a [u8], XmlError> {
        // `memchr` gives the first byte for free with runtime SIMD dispatch; the
        // full-needle check then only runs at candidate positions.
        let hay = &self.src[self.pos..];
        let first = needle[0];
        let mut i = 0usize;
        while let Some(hit) = memchr::memchr(first, &hay[i..]) {
            let at = i + hit;
            if hay[at..].starts_with(needle) {
                let out = &hay[..at];
                self.pos += at + needle.len();
                return Ok(out);
            }
            i = at + 1;
        }
        self.err(format!(
            "unterminated {:?}",
            String::from_utf8_lossy(needle)
        ))
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
    }

    /// The next event, or `None` at end of document.
    ///
    /// Not called `next`: this is not an `Iterator`, because an `Iterator` of
    /// `Result` invites `collect()` and `?`-free chaining over a stream where the
    /// FIRST error must stop the walk. A malformed mirror list that keeps
    /// yielding events builds a source list out of whatever survived the damage.
    pub fn read_event(&mut self) -> Result<Option<Event>, XmlError> {
        if let Some((name, prefix)) = self.pending_end.take() {
            return Ok(Some(Event::End { name, prefix }));
        }
        loop {
            if self.pos >= self.src.len() {
                if let Some(unclosed) = self.open.last() {
                    return self.err(format!("unclosed element <{unclosed}>"));
                }
                return Ok(None);
            }
            if self.peek() != Some(b'<') {
                let text = self.read_text()?;
                // Whitespace between elements is not content. Skipping it here
                // means a caller reading `<size> 6285 </size>` never has to
                // decide which of several Text events was the real one.
                if text.trim().is_empty() {
                    continue;
                }
                return Ok(Some(Event::Text(text)));
            }
            // `<`
            if self.starts_with(b"<!--") {
                self.pos += 4;
                self.take_until(b"-->")?;
                continue;
            }
            if self.starts_with(b"<![CDATA[") {
                self.pos += 9;
                let raw = self.take_until(b"]]>")?;
                // CDATA is literal by definition: no entity resolution.
                let s = std::str::from_utf8(raw)
                    .map_err(|_| XmlError {
                        at: self.pos,
                        why: "CDATA section is not valid UTF-8".into(),
                    })?
                    .to_string();
                if s.trim().is_empty() {
                    continue;
                }
                return Ok(Some(Event::Text(s)));
            }
            if self.starts_with(b"<?") {
                self.pos += 2;
                self.take_until(b"?>")?;
                continue;
            }
            if self.starts_with(b"<!DOCTYPE") || self.starts_with(b"<!doctype") {
                self.skip_doctype()?;
                continue;
            }
            if self.starts_with(b"</") {
                self.pos += 2;
                let (prefix, name) = self.read_name()?;
                self.skip_ws();
                if self.peek() != Some(b'>') {
                    return self.err(format!("malformed close tag for </{name}>"));
                }
                self.pos += 1;
                match self.open.pop() {
                    Some(open) if open == name => {}
                    Some(open) => {
                        return self.err(format!("</{name}> closes <{open}>"));
                    }
                    None => return self.err(format!("</{name}> with nothing open")),
                }
                self.depth -= 1;
                return Ok(Some(Event::End { name, prefix }));
            }
            self.pos += 1; // past '<'
            return self.read_start_tag();
        }
    }

    /// Skip a `<!DOCTYPE ...>`, including an internal subset, WITHOUT reading any
    /// declaration inside it.
    ///
    /// Nothing in the subset is honoured — see the module note. The only job here
    /// is to find where it ends without being fooled by a `>` inside a quoted
    /// system identifier.
    fn skip_doctype(&mut self) -> Result<(), XmlError> {
        self.pos += 9;
        let mut quote: Option<u8> = None;
        while let Some(b) = self.peek() {
            self.pos += 1;
            match (quote, b) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, b'"' | b'\'') => quote = Some(b),
                (None, b'[') => {
                    // Internal subset: skip to its close bracket, then to '>'.
                    self.take_until(b"]")?;
                }
                (None, b'>') => return Ok(()),
                _ => {}
            }
        }
        self.err("unterminated <!DOCTYPE>")
    }

    fn read_start_tag(&mut self) -> Result<Option<Event>, XmlError> {
        let (prefix, name) = self.read_name()?;
        let mut attrs: Vec<Attr> = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => return self.err(format!("unterminated <{name}>")),
                Some(b'>') => {
                    self.pos += 1;
                    self.depth += 1;
                    if self.depth > MAX_DEPTH {
                        return self.err(format!("nesting deeper than {MAX_DEPTH} elements"));
                    }
                    self.open.push(name.clone());
                    return Ok(Some(Event::Start {
                        name,
                        prefix,
                        attrs,
                    }));
                }
                Some(b'/') => {
                    self.pos += 1;
                    if self.peek() != Some(b'>') {
                        return self.err(format!("stray '/' in <{name}>"));
                    }
                    self.pos += 1;
                    // Self-closing: emit Start now, End next call. The depth is
                    // never incremented, so an empty element cannot exhaust it.
                    self.pending_end = Some((name.clone(), prefix.clone()));
                    return Ok(Some(Event::Start {
                        name,
                        prefix,
                        attrs,
                    }));
                }
                Some(_) => {
                    if attrs.len() >= MAX_ATTRS {
                        return self.err(format!("<{name}> has more than {MAX_ATTRS} attributes"));
                    }
                    attrs.push(self.read_attr()?);
                }
            }
        }
    }

    fn read_attr(&mut self) -> Result<Attr, XmlError> {
        let (prefix, name) = self.read_name()?;
        self.skip_ws();
        if self.peek() != Some(b'=') {
            return self.err(format!("attribute {name:?} has no value"));
        }
        self.pos += 1;
        self.skip_ws();
        let q = match self.peek() {
            Some(q @ (b'"' | b'\'')) => q,
            _ => return self.err(format!("attribute {name:?} value is not quoted")),
        };
        self.pos += 1;
        let raw = self.take_until(&[q])?;
        let value = decode_entities(raw, self.pos)?;
        Ok(Attr {
            name,
            prefix,
            value,
        })
    }

    /// Read a Name, splitting an optional namespace prefix off the front.
    fn read_name(&mut self) -> Result<(Option<String>, String), XmlError> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            // Deliberately permissive against the XML Name production: the
            // discriminating question here is where the name STOPS, and every
            // byte that ends one is ASCII. A non-ASCII byte inside a name is
            // passed through rather than validated, because refusing it would
            // reject documents no consumer of this parser cares about.
            if matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>' | b'=') {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return self.err("empty element or attribute name");
        }
        let raw = std::str::from_utf8(&self.src[start..self.pos]).map_err(|_| XmlError {
            at: start,
            why: "name is not valid UTF-8".into(),
        })?;
        Ok(match raw.split_once(':') {
            Some((p, n)) if !p.is_empty() && !n.is_empty() => (Some(p.to_string()), n.to_string()),
            _ => (None, raw.to_string()),
        })
    }

    fn read_text(&mut self) -> Result<String, XmlError> {
        let start = self.pos;
        let hay = &self.src[start..];
        let end = memchr::memchr(b'<', hay).unwrap_or(hay.len());
        self.pos = start + end;
        decode_entities(&hay[..end], start)
    }
}

/// Resolve the five predefined entities and numeric character references.
///
/// Any other `&name;` is an ERROR, not an empty string. A document that
/// references an entity it declared in an internal subset is asking for a
/// substitution this parser deliberately does not perform, and silently
/// dropping it would yield a mirror URL or a digest with a hole in it — the
/// worst of the three possible outcomes.
fn decode_entities(raw: &[u8], base: usize) -> Result<String, XmlError> {
    let s = std::str::from_utf8(raw).map_err(|_| XmlError {
        at: base,
        why: "character data is not valid UTF-8".into(),
    })?;
    if !s.contains('&') {
        return Ok(s.to_string());
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let semi = after.find(';').ok_or_else(|| XmlError {
            at: base + amp,
            why: "'&' with no terminating ';'".into(),
        })?;
        let name = &after[..semi];
        match name {
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "amp" => out.push('&'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => {
                let code = if let Some(hex) = name.strip_prefix("#x").or(name.strip_prefix("#X")) {
                    u32::from_str_radix(hex, 16).ok()
                } else {
                    name.strip_prefix('#').and_then(|d| d.parse::<u32>().ok())
                };
                match code.and_then(char::from_u32) {
                    Some(c) => out.push(c),
                    None => {
                        return Err(XmlError {
                            at: base + amp,
                            why: format!(
                                "entity &{name}; is not one of the five predefined entities or a \
                                 numeric character reference; this parser does not process entity \
                                 declarations"
                            ),
                        })
                    }
                }
            }
        }
        rest = &after[semi + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Look up an attribute by local name.
pub fn attr<'a>(attrs: &'a [Attr], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(name))
        .map(|a| a.value.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(src: &str) -> Result<Vec<Event>, XmlError> {
        let mut r = Reader::new(src);
        let mut out = Vec::new();
        while let Some(e) = r.read_event()? {
            out.push(e);
        }
        Ok(out)
    }

    #[test]
    fn self_closing_elements_produce_a_matching_end() {
        // The single most common defect in hand-written XML consumers: `<url/>`
        // read as an open element, so everything after it nests one level too
        // deep and the next `</resources>` closes the wrong thing.
        let ev = events("<a><b x='1'/><c/></a>").unwrap();
        assert_eq!(ev.len(), 6);
        assert!(matches!(&ev[1], Event::Start { name, .. } if name == "b"));
        assert!(matches!(&ev[2], Event::End { name, .. } if name == "b"));
        assert!(matches!(&ev[3], Event::Start { name, .. } if name == "c"));
        assert!(matches!(&ev[4], Event::End { name, .. } if name == "c"));
    }

    #[test]
    fn prefixes_are_split_so_matching_is_on_the_local_name() {
        // A real Metalink from mirrormanager carries `<mm0:timestamp>` in a
        // foreign namespace alongside the metalink elements. Matching on the raw
        // tag would miss `<metalink:file>` in a prefixed document and trip over
        // the foreign element in an unprefixed one.
        let ev = events("<mm0:timestamp>17</mm0:timestamp>").unwrap();
        match &ev[0] {
            Event::Start { name, prefix, .. } => {
                assert_eq!(name, "timestamp");
                assert_eq!(prefix.as_deref(), Some("mm0"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_five_predefined_entities_and_numeric_refs_resolve() {
        let ev = events("<a>&lt;&amp;&gt;&quot;&apos;&#65;&#x42;</a>").unwrap();
        assert_eq!(ev[1], Event::Text("<&>\"'AB".into()));
        // Query strings in mirror URLs carry `&amp;` constantly.
        let ev = events("<url>http://h/p?a=1&amp;b=2</url>").unwrap();
        assert_eq!(ev[1], Event::Text("http://h/p?a=1&b=2".into()));
    }

    #[test]
    fn an_undeclared_entity_is_an_error_rather_than_a_silent_hole() {
        // Dropping it would yield a URL or a digest missing a span of bytes,
        // which is worse than either resolving it or refusing the document.
        let e = events("<a>&fedora;</a>").unwrap_err();
        assert!(
            e.why.contains("does not process entity declarations"),
            "{e}"
        );
    }

    #[test]
    fn a_doctype_with_an_internal_subset_is_skipped_not_processed() {
        // The billion-laughs shape. There is no expansion step to bound because
        // declarations are never read at all.
        let src = "<!DOCTYPE m [<!ENTITY a \"xx\"><!ENTITY b \"&a;&a;\">]><m>ok</m>";
        let ev = events(src).unwrap();
        assert!(matches!(&ev[0], Event::Start { name, .. } if name == "m"));
        assert_eq!(ev[1], Event::Text("ok".into()));
    }

    #[test]
    fn a_gt_inside_a_quoted_system_identifier_does_not_end_the_doctype() {
        let ev = events("<!DOCTYPE m SYSTEM \"a>b.dtd\"><m/>").unwrap();
        assert!(matches!(&ev[0], Event::Start { name, .. } if name == "m"));
    }

    #[test]
    fn comments_cdata_and_processing_instructions_are_handled() {
        let src = "<?xml version='1.0'?><!-- <m> --><m><![CDATA[a<b&c]]></m>";
        let ev = events(src).unwrap();
        assert!(matches!(&ev[0], Event::Start { name, .. } if name == "m"));
        // CDATA is literal: `&c` is not an entity reference inside it.
        assert_eq!(ev[1], Event::Text("a<b&c".into()));
    }

    #[test]
    fn a_bom_does_not_hide_the_root_element() {
        let ev = events("\u{feff}<m>1</m>").unwrap();
        assert!(matches!(&ev[0], Event::Start { name, .. } if name == "m"));
    }

    #[test]
    fn mismatched_and_unclosed_tags_are_refused() {
        // Truncation is the realistic case: a mirror list cut off mid-transfer
        // must not parse as a shorter but valid list of sources.
        assert!(events("<a><b></a></b>").is_err());
        assert!(events("<a><b>text").is_err());
        assert!(events("</a>").is_err());
    }

    #[test]
    fn nesting_is_bounded() {
        let deep = "<a>".repeat(MAX_DEPTH + 2);
        let e = events(&deep).unwrap_err();
        assert!(e.why.contains("nesting deeper"), "{e}");
    }

    #[test]
    fn whitespace_between_elements_is_not_content() {
        // So a caller reading `<size> 6285 </size>` never has to work out which
        // of several Text events carried the value.
        let ev = events("<a>\n  <b>  6285  </b>\n</a>").unwrap();
        let texts: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                Event::Text(t) => Some(t.trim()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["6285"]);
    }

    #[test]
    fn attributes_read_in_either_quoting_and_by_local_name() {
        let ev = events(r#"<url protocol="http" xml:lang='en' preference="100"/>"#).unwrap();
        match &ev[0] {
            Event::Start { attrs, .. } => {
                assert_eq!(attr(attrs, "protocol"), Some("http"));
                assert_eq!(attr(attrs, "preference"), Some("100"));
                // `xml:lang` is reachable by its local name.
                assert_eq!(attr(attrs, "lang"), Some("en"));
            }
            other => panic!("{other:?}"),
        }
        assert!(events("<a b/>").is_err());
        assert!(events("<a b=c/>").is_err());
    }
}
