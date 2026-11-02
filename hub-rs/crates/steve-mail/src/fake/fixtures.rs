//! Real-world-shaped emails (`tests/fixtures/mail/*.eml`), embedded so other
//! crates' tests can use them through the `fake` feature.
//!
//! The mailbox owner in all fixtures is `me@example.com`.

macro_rules! fixtures {
    ($($name:literal),* $(,)?) => {
        /// Every fixture as `(file name, raw bytes)`.
        pub const ALL: &[(&str, &[u8])] = &[
            $(($name, include_bytes!(concat!("../../tests/fixtures/mail/", $name)))),*
        ];
    };
}

fixtures!(
    // multipart/alternative, Chinese subject split over two RFC 2047 words, encoded names, Cc
    "multipart-alternative.eml",
    // HTML only: tracking pixel, remote images, <script>, onload/onclick, iframe; ISO-8859-1 Q words
    "html-tracking.eml",
    // GB2312 From name, GBK subject, 8bit GBK body, "组织 | 职位" signature
    "gbk.eml",
    // GB18030 base64 body with a character outside GBK (U+4DAE)
    "gb18030.eml",
    // Big5 quoted-printable body
    "big5.eml",
    // ISO-2022-JP 7bit body and subject
    "iso-2022-jp.eml",
    // invoice: multipart/mixed with a PDF, RFC 2231 encoded Chinese filename
    "invoice-pdf.eml",
    // multipart/related with an inline cid: image
    "inline-cid.eml",
    // newsletter: List-Unsubscribe (+ One-Click), List-Id, Precedence: bulk, tracking pixel
    "newsletter.eml",
    // Outlook reply: top posting, "____" separator and From:/Sent:/To: block
    "outlook-quote.eml",
    // reply chain Alice -> me -> Alice; References accumulate, ">" and "在 … 写道：" quoting
    "thread-1.eml",
    "thread-2.eml",
    "thread-3.eml",
    // no Message-ID header
    "no-message-id.eml",
);

/// The raw bytes of a fixture by file name.
pub fn get(name: &str) -> Option<&'static [u8]> {
    ALL.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}

/// The three messages of the reply chain, oldest first.
pub fn thread() -> [&'static [u8]; 3] {
    [
        get("thread-1.eml").unwrap_or_default(),
        get("thread-2.eml").unwrap_or_default(),
        get("thread-3.eml").unwrap_or_default(),
    ]
}

/// A message of at least `size` bytes: short text plus a base64
/// `application/octet-stream` attachment (e.g. 28 MB for the "only fetch the
/// header and BODYSTRUCTURE" path, email.md §3.2). Deterministic.
pub fn large(size: usize) -> Vec<u8> {
    let head = "Date: Sat, 31 Oct 2026 12:00:00 +0000\r\n\
From: Scanner <scanner@office.example>\r\n\
To: me@example.com\r\n\
Subject: Scanned documents (large)\r\n\
Message-ID: <large-scan-1@office.example>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"big-b\"\r\n\
\r\n\
--big-b\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Scans attached.\r\n\
--big-b\r\n\
Content-Type: application/octet-stream; name=\"scan.bin\"\r\n\
Content-Transfer-Encoding: base64\r\n\
Content-Disposition: attachment; filename=\"scan.bin\"\r\n\
\r\n";
    let tail = "--big-b--\r\n";
    let mut out = Vec::with_capacity(size + 1024);
    out.extend_from_slice(head.as_bytes());
    // 76 base64 chars per line; content is a repeating valid base64 pattern.
    const LINE: &[u8] =
        b"QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWZnaGlqa2xtbm9wcXJzdHV2d3h5ejAxMjM0\r\n";
    while out.len() + tail.len() < size {
        out.extend_from_slice(LINE);
    }
    out.extend_from_slice(tail.as_bytes());
    out
}
