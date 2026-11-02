//! Modified UTF-7 mailbox names (RFC 3501 §5.1.3): QQ and 163 list `已发送` as `&XfJT0ZAB-`.

use base64::Engine as _;

const MUTF7: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &match base64::alphabet::Alphabet::new(
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,",
    ) {
        Ok(a) => a,
        Err(_) => panic!("static alphabet"),
    },
    base64::engine::GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
);

/// UTF-8 mailbox name → modified UTF-7 (RFC 3501 §5.1.3), as QQ/163 send `已发送`.
pub fn encode(name: &str) -> String {
    let mut out = String::new();
    let mut pending: Vec<u16> = Vec::new();
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if pending.is_empty() {
            return;
        }
        let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
        out.push('&');
        out.push_str(&MUTF7.encode(bytes));
        out.push('-');
        pending.clear();
    };
    for c in name.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut pending, &mut out);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// Modified UTF-7 → UTF-8; invalid input is returned unchanged.
pub fn decode(name: &str) -> String {
    let mut out = String::new();
    let mut rest = name;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(j) = after.find('-') else {
            return name.to_string();
        };
        let chunk = &after[..j];
        if chunk.is_empty() {
            out.push('&');
        } else {
            let Ok(bytes) = MUTF7.decode(chunk) else {
                return name.to_string();
            };
            let units: Vec<u16> = bytes
                .chunks(2)
                .filter(|c| c.len() == 2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            match String::from_utf16(&units) {
                Ok(s) => out.push_str(&s),
                Err(_) => return name.to_string(),
            }
        }
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trip() {
        for n in ["INBOX", "已发送", "Sent & Archive", "[Gmail]/所有邮件"] {
            assert_eq!(super::decode(&super::encode(n)), n);
        }
        assert_eq!(super::encode("已发送"), "&XfJT0ZAB-");
        assert_eq!(super::decode("a&-b"), "a&b");
    }
}
