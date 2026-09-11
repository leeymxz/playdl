// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: MIT OR Apache-2.0

//! URL parsing and normalization for supported schemes (`http`, `https`, `ftp`).

/// Parsed URL components required by engine network transports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Url {
    /// Lowercase scheme (`http`, `https`, or `ftp`).
    pub scheme: String,
    pub host: String,
    pub port: u16,
    /// Request path and query starting with `/`.
    pub path: String,
    /// Extracted userinfo credentials.
    pub user: Option<String>,
    pub pass: Option<String>,
}

impl Url {
    /// Returns true if this URL uses TLS.
    pub(crate) fn tls(&self) -> bool {
        self.scheme == "https"
    }

    /// Returns true if this URL uses FTP.
    pub(crate) fn is_ftp(&self) -> bool {
        self.scheme == "ftp"
    }

    /// Returns `host` or `host:port` authority string.
    pub(crate) fn authority(&self) -> String {
        let default = if self.tls() { 443 } else { 80 };
        if self.port == default {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// Returns URL string with user credentials removed.
    pub(crate) fn redacted(&self) -> String {
        let default = match self.scheme.as_str() {
            "https" => 443,
            "ftp" => 21,
            _ => 80,
        };
        if self.port == default {
            format!("{}://{}{}", self.scheme, self.host, self.path)
        } else {
            format!("{}://{}:{}{}", self.scheme, self.host, self.port, self.path)
        }
    }

    /// Extracts suggested filename from the last path segment.
    pub(crate) fn file_name(&self) -> Option<String> {
        let seg = self
            .path
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("");
        let name = percent_decode(seg);
        let base = std::path::Path::new(&name)
            .file_name()?
            .to_str()?
            .to_string();
        if base.is_empty() || base == "." || base == ".." {
            None
        } else {
            Some(base)
        }
    }

    /// Parses a URL string into components.
    pub(crate) fn parse(raw: &str) -> Result<Url, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("empty URL".into());
        }
        if raw.chars().any(|c| c.is_control()) {
            return Err("URL contains control characters".into());
        }
        let (scheme, rest) = raw
            .split_once("://")
            .ok_or_else(|| format!("{raw:?} has no scheme (want http, https or ftp)"))?;
        let scheme = scheme.to_ascii_lowercase();
        if !matches!(scheme.as_str(), "http" | "https" | "ftp") {
            return Err(format!(
                "unsupported scheme {scheme:?} (supported: {})",
                pdl_net::scheme::supported().join(", ")
            ));
        }
        let rest = rest.split('#').next().unwrap_or(rest);
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (userinfo, hostport) = match authority.rsplit_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, authority),
        };
        let (user, pass) = match userinfo {
            Some(u) => match u.split_once(':') {
                Some((a, b)) => (Some(percent_decode(a)), Some(percent_decode(b))),
                None => (Some(percent_decode(u)), None),
            },
            None => (None, None),
        };
        let default_port = match scheme.as_str() {
            "https" => 443,
            "ftp" => 21,
            _ => 80,
        };
        let (host, port) = if let Some(after) = hostport.strip_prefix('[') {
            let (h, tail) = after
                .split_once(']')
                .ok_or_else(|| format!("unterminated IPv6 literal in {raw:?}"))?;
            let p = match tail.strip_prefix(':') {
                Some(p) => p
                    .parse::<u16>()
                    .map_err(|_| format!("bad port in {raw:?}"))?,
                None => default_port,
            };
            (h.to_string(), p)
        } else {
            match hostport.rsplit_once(':') {
                Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (
                    h.to_string(),
                    p.parse::<u16>()
                        .map_err(|_| format!("bad port in {raw:?}"))?,
                ),
                _ => (hostport.to_string(), default_port),
            }
        };
        if host.is_empty() {
            return Err(format!("{raw:?} has no host"));
        }
        Ok(Url {
            scheme,
            host,
            port,
            path: path.to_string(),
            user,
            pass,
        })
    }

    /// Resolves a relative or absolute redirect target against this URL.
    pub(crate) fn join(&self, location: &str) -> Result<Url, String> {
        let loc = location.trim();
        if loc.is_empty() {
            return Err("empty Location header".into());
        }
        if loc.contains("://") {
            return Url::parse(loc);
        }
        let base = format!("{}://{}:{}", self.scheme, self.host, self.port);
        if let Some(rest) = loc.strip_prefix('/') {
            return Url::parse(&format!("{base}/{rest}"));
        }
        let dir = self.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        Url::parse(&format!("{base}{dir}/{loc}"))
    }
}

/// Decode `%XX` escapes, leaving anything malformed alone.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `Authorization: Basic` payload.
pub(crate) fn basic_auth(user: &str, pass: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let data = format!("{user}:{pass}");
    let data = data.as_bytes();
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_authority() {
        let u = Url::parse("https://example.com/a/b.iso?x=1").unwrap();
        assert_eq!(u.port, 443);
        assert_eq!(u.authority(), "example.com");
        assert_eq!(u.path, "/a/b.iso?x=1");
        assert_eq!(u.file_name().as_deref(), Some("b.iso"));

        let u = Url::parse("http://example.com:8080/x").unwrap();
        assert_eq!(u.authority(), "example.com:8080");
    }

    #[test]
    fn ipv6_literal_keeps_its_colons() {
        let u = Url::parse("http://[2001:db8::1]:8080/f").unwrap();
        assert_eq!(u.host, "2001:db8::1");
        assert_eq!(u.port, 8080);
    }

    #[test]
    fn a_redacted_url_carries_no_credentials() {
        let u = Url::parse("ftp://bob:p%40ss@files.example/pub/x.tar").unwrap();
        let r = u.redacted();
        assert_eq!(r, "ftp://files.example/pub/x.tar");
        assert!(!r.contains("bob") && !r.contains("ss"), "{r}");
        // A non-default port is load-bearing and stays.
        let u = Url::parse("http://a:b@h.example:8080/x").unwrap();
        assert_eq!(u.redacted(), "http://h.example:8080/x");
    }

    #[test]
    fn ftp_userinfo_is_extracted_and_decoded() {
        let u = Url::parse("ftp://bob:p%40ss@files.example/pub/x.tar").unwrap();
        assert!(u.is_ftp());
        assert_eq!(u.port, 21);
        assert_eq!(u.user.as_deref(), Some("bob"));
        assert_eq!(u.pass.as_deref(), Some("p@ss"));
    }

    #[test]
    fn hostile_inputs_are_refused_rather_than_normalised() {
        for bad in [
            "",
            "example.com/x",
            "gopher://example.com/x",
            "https:///nohost",
            "https://exa\r\nmple.com/x",
        ] {
            assert!(Url::parse(bad).is_err(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn a_server_supplied_name_cannot_escape_a_directory() {
        let u = Url::parse("https://example.com/a/%2e%2e%2f%2e%2e%2fetc%2fpasswd").unwrap();
        assert_eq!(u.file_name().as_deref(), Some("passwd"));
        let u = Url::parse("https://example.com/a/").unwrap();
        assert_eq!(u.file_name(), None);
    }

    #[test]
    fn redirects_resolve_in_all_three_forms() {
        let base = Url::parse("https://a.example/dir/file").unwrap();
        assert_eq!(base.join("https://b.example/x").unwrap().host, "b.example");
        assert_eq!(base.join("/root").unwrap().path, "/root");
        assert_eq!(base.join("sib").unwrap().path, "/dir/sib");
    }

    #[test]
    fn basic_auth_matches_rfc_7617_examples() {
        assert_eq!(
            basic_auth("Aladdin", "open sesame"),
            "QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
    }
}
