//! The sign-in code's address (decision 47): `<server>/link/czl_<code>`,
//! from the QR code, a camera app's deep link, or pasted.

/// The server's address and the code, or `None` when this is not a CalZone
/// sign-in address. Surrounding spaces, a trailing slash and anything after
/// `?` or `#` are ignored.
pub fn parse(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let url = url.split(['?', '#']).next().unwrap_or("").trim_end_matches('/');
    let (server, code) = url.rsplit_once("/link/")?;
    if !(server.starts_with("https://") || server.starts_with("http://")) {
        return None;
    }
    let rest = code.strip_prefix("czl_")?;
    let ok = rest.len() == 43 && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    ok.then(|| (server.to_string(), code.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_in_addresses() {
        let code = format!("czl_{}", "a".repeat(43));
        assert_eq!(
            parse(&format!(" https://calzone.example.com/link/{code}/ ")),
            Some(("https://calzone.example.com".into(), code.clone()))
        );
        assert_eq!(
            parse(&format!("https://example.com/calzone/link/{code}?x=1")),
            Some(("https://example.com/calzone".into(), code.clone()))
        );
        assert_eq!(parse(&format!("http://192.168.1.2:8080/link/{code}")).unwrap().0, "http://192.168.1.2:8080");
        assert_eq!(parse(&format!("calzone.example.com/link/{code}")), None);
        assert_eq!(parse("https://calzone.example.com/link/czl_short"), None);
        assert_eq!(parse("https://calzone.example.com/calendar"), None);
    }
}
