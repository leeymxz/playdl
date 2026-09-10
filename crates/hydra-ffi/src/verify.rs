// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Post-transfer file integrity verification.

use crate::abi::hydra_error_code_t as E;
use crate::engine::Algo;
use crate::err::{self, Detail};
use std::io::Read;

/// File reading buffer size (1 MiB).
const CHUNK: usize = 1 << 20;

/// Computes the hash digest of `path` using `algo`.
fn digest_file(path: &str, algo: Algo) -> Result<Vec<u8>, Detail> {
    use md5::Digest as _;
    let mut f = std::fs::File::open(path).map_err(|e| err::from_io(&e))?;
    let mut buf = vec![0u8; CHUNK];

    macro_rules! run {
        ($h:expr) => {{
            let mut h = $h;
            loop {
                match f.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => h.update(&buf[..n]),
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(err::from_io(&e)),
                }
            }
            h.finalize().to_vec()
        }};
    }

    Ok(match algo {
        Algo::Md5 => run!(md5::Md5::new()),
        Algo::Sha1 => run!(sha1::Sha1::new()),
        Algo::Sha256 => run!(sha2::Sha256::new()),
        Algo::Sha512 => run!(sha2::Sha512::new()),
        Algo::Blake3 => {
            let mut h = blake3::Hasher::new();
            loop {
                match f.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        h.update(&buf[..n]);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(err::from_io(&e)),
                }
            }
            h.finalize().as_bytes().to_vec()
        }
    })
}

/// Verifies that the file at `path` matches the expected `want` digest.
pub(crate) fn check(path: &str, algo: Algo, want: &[u8]) -> Result<(), Detail> {
    let got = digest_file(path, algo).map_err(|mut d| {
        d.code = E::HYDRA_ERR_VERIFICATION as u32;
        d.message = format!("cannot verify {path}: {}", d.message);
        d
    })?;
    if got == want {
        return Ok(());
    }
    Err(Detail {
        code: E::HYDRA_ERR_CHECKSUM as u32,
        os_error: 0,
        http_status: 0,
        message: format!(
            "{} mismatch: expected {}, got {}",
            algo.as_str(),
            pdl_net::digest::to_lower_hex(want),
            pdl_net::digest::to_lower_hex(&got)
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, body: &[u8]) -> String {
        let p = std::env::temp_dir().join(format!("hydra-ffi-verify-{name}"));
        std::fs::write(&p, body).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn every_algorithm_matches_its_known_answer_for_abc() {
        let p = tmp("abc", b"abc");
        // Published test vectors, so a wrong wiring of algorithm to hasher is
        // caught rather than being self-consistent.
        let cases: &[(Algo, &str)] = &[
            (Algo::Md5, "900150983cd24fb0d6963f7d28e17f72"),
            (Algo::Sha1, "a9993e364706816aba3e25717850c26c9cd0d89d"),
            (
                Algo::Sha256,
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                Algo::Blake3,
                "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85",
            ),
        ];
        for (algo, hex) in cases {
            let want: Vec<u8> = (0..hex.len() / 2)
                .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
                .collect();
            assert_eq!(want.len(), algo.len(), "{} vector length", algo.as_str());
            check(&p, *algo, &want).unwrap_or_else(|e| panic!("{}: {}", algo.as_str(), e.message));
        }
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn a_mismatch_and_a_missing_file_are_different_failures() {
        let p = tmp("mismatch", b"abc");
        let wrong = [0u8; 32];
        let e = check(&p, Algo::Sha256, &wrong).unwrap_err();
        assert_eq!(e.code, E::HYDRA_ERR_CHECKSUM as u32);
        std::fs::remove_file(&p).ok();

        let e = check(&p, Algo::Sha256, &wrong).unwrap_err();
        assert_eq!(
            e.code,
            E::HYDRA_ERR_VERIFICATION as u32,
            "an unreadable file must not be reported as corrupt data"
        );
    }
}
