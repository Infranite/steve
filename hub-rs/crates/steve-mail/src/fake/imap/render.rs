//! FETCH data items that need the MIME tree: ENVELOPE, BODY/BODYSTRUCTURE
//! and `BODY[section]`.

use mail_parser::{Message, MessageParser, MessagePart};

use super::parse::{istr, nstr};
use crate::fake::mime::MailMeta;

fn addr_list(list: &[(String, String)]) -> Vec<u8> {
    if list.is_empty() {
        return b"NIL".to_vec();
    }
    let mut out = b"(".to_vec();
    for (name, addr) in list {
        let (local, host) = addr.rsplit_once('@').unwrap_or((addr.as_str(), ""));
        out.push(b'(');
        out.extend(nstr((!name.is_empty()).then_some(name.as_bytes())));
        out.extend_from_slice(b" NIL ");
        out.extend(istr(local.as_bytes()));
        out.push(b' ');
        out.extend(istr(host.as_bytes()));
        out.push(b')');
    }
    out.push(b')');
    out
}

/// `ENVELOPE` (RFC 3501 §7.4.2). Sender and Reply-To default to From.
pub(crate) fn envelope(meta: &MailMeta) -> Vec<u8> {
    let from: Vec<(String, String)> = meta.from.iter().cloned().collect();
    let reply_to = if meta.reply_to.is_empty() {
        &from
    } else {
        &meta.reply_to
    };
    let mut out = b"(".to_vec();
    out.extend(nstr(meta.raw_date.as_deref().map(str::as_bytes)));
    out.push(b' ');
    out.extend(nstr(meta.raw_subject.as_deref().map(str::as_bytes)));
    for list in [&from, &from, reply_to, &meta.to, &meta.cc, &meta.bcc] {
        out.push(b' ');
        out.extend(addr_list(list));
    }
    out.push(b' ');
    let irt = (!meta.in_reply_to.is_empty()).then(|| {
        meta.in_reply_to
            .iter()
            .map(|i| format!("<{i}>"))
            .collect::<Vec<_>>()
            .join(" ")
    });
    out.extend(nstr(irt.as_deref().map(str::as_bytes)));
    out.push(b' ');
    let mid = meta.message_id.as_ref().map(|m| format!("<{m}>"));
    out.extend(nstr(mid.as_deref().map(str::as_bytes)));
    out.push(b')');
    out
}

/// Children in IMAP part numbering: a multipart's parts; for an embedded
/// message, its body's parts (or the body itself when it is not multipart).
fn numbered_children<'m>(p: &MessagePart<'m>) -> Vec<MessagePart<'m>> {
    if p.is_multipart() {
        return p.children().collect();
    }
    if let Some(n) = p.nested() {
        let r = n.root_part();
        return if r.is_multipart() {
            r.children().collect()
        } else {
            vec![r]
        };
    }
    Vec::new()
}

fn find_part<'m>(msg: &'m Message<'m>, path: &[u32]) -> Option<MessagePart<'m>> {
    let root = msg.root().root_part();
    let mut level = if root.is_multipart() {
        root.children().collect()
    } else {
        vec![root]
    };
    let mut cur = None;
    for (i, n) in path.iter().enumerate() {
        let p = *level.get((*n as usize).checked_sub(1)?)?;
        if i + 1 < path.len() {
            level = numbered_children(&p);
        }
        cur = Some(p);
    }
    cur
}

fn split_header(raw: &[u8]) -> usize {
    raw.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
        .unwrap_or(raw.len())
}

/// Header lines whose name is (or with `not`, is not) in `names`, plus the
/// terminating blank line.
fn header_fields(header: &[u8], names: &[String], not: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut keep = false;
    for line in header.split_inclusive(|&c| c == b'\n') {
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        if !matches!(line.first(), Some(b' ') | Some(b'\t')) {
            let name = line
                .split(|&c| c == b':')
                .next()
                .map(|n| String::from_utf8_lossy(n).trim().to_ascii_uppercase())
                .unwrap_or_default();
            keep = names.contains(&name) != not;
        }
        if keep {
            out.extend_from_slice(line);
        }
    }
    out.extend_from_slice(b"\r\n");
    out
}

/// The bytes of `BODY[<spec>]`, `spec` being the text between the brackets.
pub(crate) fn section(raw: &[u8], spec: &str) -> Option<Vec<u8>> {
    let spec = spec.trim();
    let (path, rest) = {
        let mut nums = Vec::new();
        let mut rest = spec;
        while let Some(c) = rest.chars().next() {
            if !c.is_ascii_digit() {
                break;
            }
            let end = rest.find('.').unwrap_or(rest.len());
            nums.push(rest[..end].parse::<u32>().ok()?);
            rest = rest.get(end + 1..).unwrap_or("");
        }
        (nums, rest.to_ascii_uppercase())
    };
    if path.is_empty() {
        return text_section(raw, &rest);
    }
    let msg = MessageParser::new().parse(raw)?;
    let part = find_part(&msg, &path)?;
    match rest.as_str() {
        "" => Some(part.raw_body().to_vec()),
        "MIME" => Some(part.raw_headers().to_vec()),
        _ => {
            let nested = part.nested()?;
            text_section(nested.raw(), &rest)
        }
    }
}

fn text_section(raw: &[u8], rest: &str) -> Option<Vec<u8>> {
    let split = split_header(raw);
    if rest.is_empty() {
        return Some(raw.to_vec());
    }
    if rest == "HEADER" {
        return Some(raw[..split].to_vec());
    }
    if rest == "TEXT" {
        return Some(raw[split..].to_vec());
    }
    let (not, list) = if let Some(l) = rest.strip_prefix("HEADER.FIELDS.NOT") {
        (true, l)
    } else {
        (false, rest.strip_prefix("HEADER.FIELDS")?)
    };
    let names: Vec<String> = list
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .split_whitespace()
        .map(|n| n.trim_matches('"').to_ascii_uppercase())
        .collect();
    Some(header_fields(&raw[..split], &names, not))
}

fn params(p: &MessagePart<'_>, skip_boundary: bool) -> Vec<u8> {
    let Some(ct) = p.content_type() else {
        return if p.is_text() {
            b"(\"CHARSET\" \"US-ASCII\")".to_vec()
        } else {
            b"NIL".to_vec()
        };
    };
    let attrs: Vec<(&str, &str)> = ct
        .attributes()
        .filter(|(n, _)| !(skip_boundary && *n == "boundary"))
        .collect();
    if attrs.is_empty() {
        return b"NIL".to_vec();
    }
    let mut out = b"(".to_vec();
    for (i, (n, v)) in attrs.iter().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        out.extend(istr(n.to_ascii_uppercase().as_bytes()));
        out.push(b' ');
        out.extend(istr(v.as_bytes()));
    }
    out.push(b')');
    out
}

fn disposition(p: &MessagePart<'_>) -> Vec<u8> {
    let Some(d) = p.content_disposition() else {
        return b"NIL".to_vec();
    };
    let mut out = b"(".to_vec();
    out.extend(istr(d.ctype().to_ascii_uppercase().as_bytes()));
    out.push(b' ');
    let attrs: Vec<_> = d.attributes().collect();
    if attrs.is_empty() {
        out.extend_from_slice(b"NIL");
    } else {
        out.push(b'(');
        for (i, (n, v)) in attrs.iter().enumerate() {
            if i > 0 {
                out.push(b' ');
            }
            out.extend(istr(n.to_ascii_uppercase().as_bytes()));
            out.push(b' ');
            out.extend(istr(v.as_bytes()));
        }
        out.push(b')');
    }
    out.push(b')');
    out
}

fn structure(p: &MessagePart<'_>, ext: bool) -> Vec<u8> {
    let mut out = b"(".to_vec();
    let ct = p.content_type();
    if p.is_multipart() {
        for c in p.children() {
            out.extend(structure(&c, ext));
        }
        let sub = ct
            .and_then(|c| c.subtype())
            .unwrap_or("mixed")
            .to_ascii_uppercase();
        out.push(b' ');
        out.extend(istr(sub.as_bytes()));
        if ext {
            out.push(b' ');
            let boundary = p.boundary().unwrap_or_default();
            out.extend_from_slice(b"(\"BOUNDARY\" ");
            out.extend(istr(boundary.as_bytes()));
            out.extend_from_slice(b") NIL NIL NIL");
        }
        out.push(b')');
        return out;
    }
    let (ty, sub) = match ct {
        Some(c) => (
            c.ctype().to_ascii_uppercase(),
            c.subtype().unwrap_or("plain").to_ascii_uppercase(),
        ),
        None => ("TEXT".to_string(), "PLAIN".to_string()),
    };
    let body = p.raw_body();
    out.extend(istr(ty.as_bytes()));
    out.push(b' ');
    out.extend(istr(sub.as_bytes()));
    out.push(b' ');
    out.extend(params(p, false));
    out.push(b' ');
    let cid = p
        .content_id()
        .map(|c| format!("<{}>", crate::fake::bare_id(c)));
    out.extend(nstr(cid.as_deref().map(str::as_bytes)));
    out.push(b' ');
    out.extend(nstr(p.content_description().map(str::as_bytes)));
    out.push(b' ');
    let enc = p
        .content_transfer_encoding()
        .unwrap_or("7BIT")
        .trim()
        .to_ascii_uppercase();
    out.extend(istr(enc.as_bytes()));
    out.extend(format!(" {}", body.len()).into_bytes());
    let lines = body.iter().filter(|&&c| c == b'\n').count();
    if let Some(nested) = p.nested() {
        out.push(b' ');
        out.extend(envelope(&MailMeta::parse(nested.raw())));
        out.push(b' ');
        out.extend(structure(&nested.root_part(), ext));
        out.extend(format!(" {lines}").into_bytes());
    } else if ty == "TEXT" {
        out.extend(format!(" {lines}").into_bytes());
    }
    if ext {
        out.extend_from_slice(b" NIL ");
        out.extend(disposition(p));
        out.extend_from_slice(b" NIL NIL");
    }
    out.push(b')');
    out
}

/// `BODY` (`ext = false`) or `BODYSTRUCTURE` (`ext = true`).
pub(crate) fn body_structure(raw: &[u8], ext: bool) -> Vec<u8> {
    match MessageParser::new().parse(raw) {
        Some(msg) => structure(&msg.root().root_part(), ext),
        None => b"(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"US-ASCII\") NIL NIL \"7BIT\" 0 0)".to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::fixtures;

    #[test]
    fn sections() {
        let raw = fixtures::get("invoice-pdf.eml").unwrap();
        let h = section(raw, "HEADER").unwrap();
        assert!(h.ends_with(b"\r\n\r\n"));
        let s = section(raw, "HEADER.FIELDS (SUBJECT MESSAGE-ID)").unwrap();
        let s = String::from_utf8(s).unwrap();
        assert!(s.starts_with("Subject:") && s.contains("Message-ID:") && !s.contains("From:"));
        let pdf = section(raw, "2").unwrap();
        assert!(pdf.starts_with(b"JVBERi0x"), "base64 of %PDF-1");
        let mime = String::from_utf8(section(raw, "2.MIME").unwrap()).unwrap();
        assert!(mime.contains("application/pdf"));
        assert_eq!(section(raw, "").unwrap(), raw);
    }

    #[test]
    fn structure_of_mixed() {
        let raw = fixtures::get("invoice-pdf.eml").unwrap();
        let s = String::from_utf8_lossy(&body_structure(raw, true)).into_owned();
        assert!(s.contains("\"APPLICATION\" \"PDF\""), "{s}");
        assert!(s.contains("\"MIXED\""), "{s}");
        assert!(s.contains("\"ATTACHMENT\""), "{s}");
    }
}
