//! Wire-level pieces of the fake IMAP server: tokenizer, sequence sets,
//! dates, strings and modified UTF-7 mailbox names.

use base64::Engine;

/// One token of a command line. Quoted strings and literals are both `Str`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Atom(String),
    Str(Vec<u8>),
    List(Vec<Value>),
}

impl Value {
    /// Atom or string as text (lossy for 8-bit strings).
    pub(crate) fn text(&self) -> Option<String> {
        match self {
            Value::Atom(a) => Some(a.clone()),
            Value::Str(s) => Some(String::from_utf8_lossy(s).into_owned()),
            Value::List(_) => None,
        }
    }
    pub(crate) fn atom_upper(&self) -> Option<String> {
        match self {
            Value::Atom(a) => Some(a.to_ascii_uppercase()),
            _ => None,
        }
    }
}

/// Tokenizes the arguments of a command (everything after the command name).
/// Literals arrive inline as `{n}\r\n<n bytes>` (or `{n+}`), exactly as on
/// the wire.
pub(crate) fn tokenize(input: &[u8]) -> Result<Vec<Value>, String> {
    let mut p = Parser { s: input, i: 0 };
    let v = p.list_items(false)?;
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn list_items(&mut self, nested: bool) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        loop {
            while matches!(self.peek(), Some(b' ')) {
                self.i += 1;
            }
            match self.peek() {
                None | Some(b'\r') | Some(b'\n') => {
                    if nested {
                        return Err("unterminated list".into());
                    }
                    return Ok(out);
                }
                Some(b')') => {
                    if !nested {
                        return Err("unexpected )".into());
                    }
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'(') => {
                    self.i += 1;
                    out.push(Value::List(self.list_items(true)?));
                }
                Some(b'"') => out.push(Value::Str(self.quoted()?)),
                Some(b'{') => out.push(Value::Str(self.literal()?)),
                Some(_) => out.push(Value::Atom(self.atom()?)),
            }
        }
    }

    fn quoted(&mut self) -> Result<Vec<u8>, String> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None | Some(b'\r') | Some(b'\n') => return Err("unterminated string".into()),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = self.peek().ok_or("bad escape")?;
                    out.push(c);
                    self.i += 1;
                }
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    fn literal(&mut self) -> Result<Vec<u8>, String> {
        let close = self.s[self.i..]
            .iter()
            .position(|&c| c == b'}')
            .ok_or("bad literal")?
            + self.i;
        let spec = std::str::from_utf8(&self.s[self.i + 1..close]).map_err(|_| "bad literal")?;
        let n: usize = spec
            .trim_end_matches('+')
            .parse()
            .map_err(|_| "bad literal size")?;
        let mut j = close + 1;
        if self.s.get(j) == Some(&b'\r') {
            j += 1;
        }
        if self.s.get(j) == Some(&b'\n') {
            j += 1;
        }
        let end = j.checked_add(n).ok_or("bad literal size")?;
        let body = self.s.get(j..end).ok_or("short literal")?.to_vec();
        self.i = end;
        Ok(body)
    }

    /// Atoms may embed `[...]` (with spaces and parens, as in
    /// `BODY.PEEK[HEADER.FIELDS (SUBJECT)]<0.100>`).
    fn atom(&mut self) -> Result<String, String> {
        let start = self.i;
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            match c {
                b'[' => depth += 1,
                b']' => depth = depth.saturating_sub(1),
                b' ' | b'(' | b')' | b'\r' | b'\n' | b'"' | b'{' if depth == 0 => break,
                b'\r' | b'\n' => break,
                _ => {}
            }
            self.i += 1;
        }
        if self.i == start {
            return Err("empty atom".into());
        }
        Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }
}

/// An IMAP sequence set (`1:3,5,7:*`); `u32::MAX` stands for `*`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SeqSet(Vec<(u32, u32)>);

impl SeqSet {
    pub(crate) fn all() -> Self {
        Self(vec![(1, u32::MAX)])
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        let mut out = Vec::new();
        for part in s.split(',') {
            let num = |x: &str| -> Option<u32> {
                if x == "*" {
                    Some(u32::MAX)
                } else {
                    x.parse::<u32>().ok().filter(|n| *n > 0)
                }
            };
            match part.split_once(':') {
                Some((a, b)) => out.push((num(a)?, num(b)?)),
                None => {
                    let n = num(part)?;
                    out.push((n, n));
                }
            }
        }
        (!out.is_empty()).then_some(Self(out))
    }

    /// RFC 3501: `*` is the largest number in use, and `a:b` equals `b:a` —
    /// so `UID FETCH 101:*` on a mailbox whose last UID is 100 still returns
    /// UID 100. Real clients must cope; the fake does the same.
    pub(crate) fn contains(&self, n: u32, max: u32) -> bool {
        self.0.iter().any(|&(a, b)| {
            let a = if a == u32::MAX { max } else { a };
            let b = if b == u32::MAX { max } else { b };
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            (lo..=hi).contains(&n)
        })
    }

    /// Compact text form of a list of numbers (for COPYUID).
    pub(crate) fn format(nums: &[u32]) -> String {
        let mut out: Vec<String> = Vec::new();
        let mut i = 0;
        while i < nums.len() {
            let start = nums[i];
            let mut end = start;
            while i + 1 < nums.len() && nums[i + 1] == end + 1 {
                i += 1;
                end = nums[i];
            }
            out.push(if start == end {
                start.to_string()
            } else {
                format!("{start}:{end}")
            });
            i += 1;
        }
        out.join(",")
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub(crate) fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `1-Feb-1994` → days since the epoch.
pub(crate) fn parse_date(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let d: u32 = it.next()?.trim().parse().ok()?;
    let mon = it.next()?;
    let m = MONTHS.iter().position(|x| x.eq_ignore_ascii_case(mon))? as u32 + 1;
    let y: i64 = it.next()?.trim().parse().ok()?;
    Some(days_from_civil(y, m, d))
}

/// `17-Jul-1996 02:44:25 -0700` → unix seconds.
pub(crate) fn parse_date_time(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once(' ')?;
    let (time, zone) = rest.trim().split_once(' ')?;
    let days = parse_date(date.trim())?;
    let mut t = time.split(':').map(|x| x.parse::<i64>().ok());
    let (h, mi, se) = (t.next()??, t.next()??, t.next()??);
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let z: i64 = zone.trim_start_matches(['+', '-']).parse().ok()?;
    let offset = sign * ((z / 100) * 3600 + (z % 100) * 60);
    Some(days * 86400 + h * 3600 + mi * 60 + se - offset)
}

/// Unix seconds → `26-Oct-2026 09:12:00 +0000` (always UTC).
pub(crate) fn format_date_time(ts: i64) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(86400));
    let secs = ts.rem_euclid(86400);
    format!(
        "{:02}-{}-{:04} {:02}:{:02}:{:02} +0000",
        d,
        MONTHS[(m - 1) as usize],
        y,
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

/// An IMAP string: quoted when it can be, a literal otherwise (8-bit or CR/LF).
pub(crate) fn istr(s: &[u8]) -> Vec<u8> {
    if s.iter().all(|&c| (0x20..0x7f).contains(&c)) {
        let mut out = Vec::with_capacity(s.len() + 2);
        out.push(b'"');
        for &c in s {
            if c == b'"' || c == b'\\' {
                out.push(b'\\');
            }
            out.push(c);
        }
        out.push(b'"');
        out
    } else {
        let mut out = format!("{{{}}}\r\n", s.len()).into_bytes();
        out.extend_from_slice(s);
        out
    }
}

/// `istr` or `NIL`.
pub(crate) fn nstr(s: Option<&[u8]>) -> Vec<u8> {
    match s {
        Some(s) => istr(s),
        None => b"NIL".to_vec(),
    }
}

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
pub(crate) fn mutf7_encode(name: &str) -> String {
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
pub(crate) fn mutf7_decode(name: &str) -> String {
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

/// IMAP LIST wildcard match (`*` matches anything, `%` anything but `/`).
pub(crate) fn wildcard(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|k| go(&p[1..], &n[k..])),
            Some('%') => (0..=n.len())
                .take_while(|&k| k == 0 || n[k - 1] != '/')
                .any(|k| go(&p[1..], &n[k..])),
            Some(c) => n.first() == Some(c) && go(&p[1..], &n[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_sections_and_literals() {
        let v = tokenize(
            b"1:* (UID BODY.PEEK[HEADER.FIELDS (SUBJECT FROM)]<0.100>) {3}\r\nabc \"q\\\"x\"",
        )
        .unwrap();
        assert_eq!(v[0], Value::Atom("1:*".into()));
        let Value::List(items) = &v[1] else { panic!() };
        assert_eq!(
            items[1],
            Value::Atom("BODY.PEEK[HEADER.FIELDS (SUBJECT FROM)]<0.100>".into())
        );
        assert_eq!(v[2], Value::Str(b"abc".to_vec()));
        assert_eq!(v[3], Value::Str(b"q\"x".to_vec()));
    }

    #[test]
    fn seq_sets_follow_rfc() {
        let s = SeqSet::parse("101:*").unwrap();
        assert!(s.contains(100, 100), "n:* with n > max still matches max");
        assert!(!s.contains(99, 100));
        let s = SeqSet::parse("1,3:4").unwrap();
        assert!(s.contains(3, 9) && !s.contains(2, 9));
        assert_eq!(SeqSet::format(&[1, 2, 3, 5, 7, 8]), "1:3,5,7:8");
    }

    #[test]
    fn dates_round_trip() {
        let ts = parse_date_time("26-Oct-2026 09:12:00 +0800").unwrap();
        assert_eq!(format_date_time(ts), "26-Oct-2026 01:12:00 +0000");
        assert_eq!(parse_date("1-Jan-1970"), Some(0));
    }

    #[test]
    fn mutf7() {
        assert_eq!(mutf7_encode("已发送"), "&XfJT0ZAB-");
        assert_eq!(mutf7_decode("&XfJT0ZAB-"), "已发送");
        assert_eq!(mutf7_encode("A&B"), "A&-B");
        assert_eq!(mutf7_decode("A&-B"), "A&B");
    }

    #[test]
    fn wildcards() {
        assert!(wildcard("*", "[Gmail]/Sent Mail"));
        assert!(!wildcard("%", "[Gmail]/Sent Mail"));
        assert!(wildcard("[Gmail]/%", "[Gmail]/Sent Mail"));
    }
}
