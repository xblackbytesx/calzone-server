//! HTTP authentication for the Baikal importer: Basic, or Digest with MD5
//! (RFC 7616 / RFC 2617, `qop=auth`), which is Baikal's default.

use base64::Engine;
use md5::{Digest, Md5};

/// What the server asked for in `WWW-Authenticate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Challenge {
    Basic,
    Digest { realm: String, nonce: String, opaque: Option<String>, qop_auth: bool },
}

/// The best challenge in a set of `WWW-Authenticate` values: Digest (MD5)
/// over Basic, since Digest never sends the password itself.
pub fn pick<'a>(headers: impl Iterator<Item = &'a str>) -> Option<Challenge> {
    let mut basic = false;
    for h in headers {
        let (scheme, rest) = h.trim().split_once(' ').unwrap_or((h.trim(), ""));
        if scheme.eq_ignore_ascii_case("basic") {
            basic = true;
        } else if scheme.eq_ignore_ascii_case("digest") {
            let params = params(rest);
            let get = |k: &str| params.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone());
            let algorithm = get("algorithm").unwrap_or_else(|| "MD5".into());
            if !algorithm.eq_ignore_ascii_case("MD5") {
                continue;
            }
            let (Some(realm), Some(nonce)) = (get("realm"), get("nonce")) else { continue };
            let qop_auth = get("qop").is_some_and(|q| q.split(',').any(|v| v.trim().eq_ignore_ascii_case("auth")));
            return Some(Challenge::Digest { realm, nonce, opaque: get("opaque"), qop_auth });
        }
    }
    basic.then_some(Challenge::Basic)
}

/// `name=value` and `name="quoted, value"` pairs, comma separated.
fn params(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let Some((name, after)) = rest.split_once('=') else { break };
        let name = name.trim().trim_start_matches(',').trim().to_string();
        let after = after.trim_start();
        let (value, tail) = if let Some(q) = after.strip_prefix('"') {
            let mut v = String::new();
            let mut chars = q.char_indices();
            let mut end = q.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, n)) = chars.next() {
                            v.push(n);
                        }
                    }
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    c => v.push(c),
                }
            }
            (v, &q[end.min(q.len())..])
        } else {
            let end = after.find(',').unwrap_or(after.len());
            (after[..end].trim().to_string(), &after[end..])
        };
        out.push((name, value));
        rest = tail.trim_start().trim_start_matches(',').trim_start();
    }
    out
}

fn md5_hex(s: &str) -> String {
    Md5::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn basic(user: &str, password: &str) -> String {
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}")))
}

/// The `Authorization` value for one Digest request. `nc` counts requests
/// made with this nonce, from 1.
#[allow(clippy::too_many_arguments)]
pub fn digest(
    user: &str,
    password: &str,
    method: &str,
    uri: &str,
    realm: &str,
    nonce: &str,
    opaque: Option<&str>,
    qop_auth: bool,
    nc: u32,
    cnonce: &str,
) -> String {
    let ha1 = md5_hex(&format!("{user}:{realm}:{password}"));
    let ha2 = md5_hex(&format!("{method}:{uri}"));
    let nc = format!("{nc:08x}");
    let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut h =
        format!("Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\"", q(user), q(realm), q(nonce), q(uri));
    if qop_auth {
        let response = md5_hex(&format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}"));
        h.push_str(&format!(", qop=auth, nc={nc}, cnonce=\"{cnonce}\", response=\"{response}\""));
    } else {
        let response = md5_hex(&format!("{ha1}:{nonce}:{ha2}"));
        h.push_str(&format!(", response=\"{response}\""));
    }
    h.push_str(", algorithm=MD5");
    if let Some(o) = opaque {
        h.push_str(&format!(", opaque=\"{}\"", q(o)));
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_2617_example() {
        let h = digest(
            "Mufasa",
            "Circle Of Life",
            "GET",
            "/dir/index.html",
            "testrealm@host.com",
            "dcd98b7102dd2f0e8b11d0f600bfb0c093",
            Some("5ccc069c403ebaf9f0171e9517f40e41"),
            true,
            1,
            "0a4f113b",
        );
        assert!(h.contains("response=\"6629fae49393a05397450978507c4ef1\""), "{h}");
        assert!(h.contains("nc=00000001") && h.contains("opaque=\"5ccc069c403ebaf9f0171e9517f40e41\""));
    }

    #[test]
    fn challenges() {
        let sabre = r#"Digest realm="BaikalDAV",qop="auth",nonce="5f1a",opaque="c0ff""#;
        assert_eq!(
            pick([r#"Basic realm="x""#, sabre].into_iter()),
            Some(Challenge::Digest {
                realm: "BaikalDAV".into(),
                nonce: "5f1a".into(),
                opaque: Some("c0ff".into()),
                qop_auth: true
            })
        );
        assert_eq!(pick([r#"Basic realm="CalZone""#].into_iter()), Some(Challenge::Basic));
        assert_eq!(pick([r#"Digest realm="a", nonce="b", algorithm=SHA-512-256"#].into_iter()), None);
        assert_eq!(pick(["Bearer"].into_iter()), None);
        let p = params(r#"realm="a, \"b\"", nonce=xyz , qop="auth,auth-int""#);
        assert_eq!(p[0], ("realm".into(), "a, \"b\"".into()));
        assert_eq!(p[1], ("nonce".into(), "xyz".into()));
        assert_eq!(p[2].1, "auth,auth-int");
        assert_eq!(basic("Aladdin", "open sesame"), "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
    }
}
