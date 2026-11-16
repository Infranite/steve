"""Synthetic data for delivery-hub.browser.mjs, only inside the installation that test creates."""
import base64
import hashlib
import pathlib
import secrets
import sqlite3
import sys
import time
import zipfile

home, action = pathlib.Path(sys.argv[1]), sys.argv[2]
assert (home / ".delivery-browser-test").is_file(), "not a delivery test installation"
c = sqlite3.connect(home / "data/steve.sqlite3", timeout=20)
now = int(time.time() * 1000)
user = c.execute("SELECT id FROM users WHERE subject='dev:local'").fetchone()[0]
if action == "remove-membership":
    c.execute("DELETE FROM workspace_members WHERE workspace_id='default' AND user_id=?", (user,))
elif action == "restore-membership":
    c.execute("INSERT OR IGNORE INTO workspace_members(workspace_id,user_id,role,created) VALUES('default',?,'owner',?)", (user, now))
elif action == "seed":
    c.execute("INSERT INTO workspaces(id,name,created) VALUES('delivery-b','Delivery B',?)", (now,))
    c.execute("INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES('delivery-b',?,'owner',?)", (user, now))
    for ws, item in [("default", "ST-1"), ("delivery-b", "B-1")]:
        source = home / "source" / ws
        source.mkdir(parents=True)
        c.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES(?,?,1,?,'run','work',?,?)", (item, ws, "Delivery browser " + ws, now, now))
        c.execute("INSERT INTO item_goals(item_id,version,text,created) VALUES(?,1,'Test immutable file delivery',?)", (item, now))
        token = secrets.token_hex(32)
        (home / (ws + ".token")).write_text(token)
        (home / (ws + ".token")).chmod(0o600)
        c.execute("INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,token_hash,title,created,updated) VALUES(?,?,?,?,?,'running',?,'Delivery browser',?,?)", ("delivery-" + ws, ws, item, "test", str(source), hashlib.sha256(token.encode()).hexdigest(), now, now))
        c.execute("INSERT INTO messages(id,workspace_id,space,author,text,item_id,created,meta) VALUES(?,?,'steve','user','Please deliver these test files',?,?, '{}')", ("origin-" + ws, ws, item, now))
        c.execute("INSERT INTO thread_items(item_id,workspace_id,origin_msg,created) VALUES(?,?,?,?)", (item, ws, "origin-" + ws, now))
        (source / "report.md").write_text("# Delivered report\n\nImmutable **Markdown** from the real delivery tool.\n")
        (source / "pixel.png").write_bytes(base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+j1ioAAAAASUVORK5CYII="))
        html = '''<!doctype html><link rel="stylesheet" href="../assets/theme.css"><h1 id="marker">WORKSPACE</h1><img id="picture" alt="Bundled pixel" src="../assets/pixel.png"><a id="next" href="next.html">Next page</a><button id="interaction" onclick="this.textContent='Script executed'">Run script</button><script>window.executed=true;parent.document.body.dataset.compromised='yes';fetch('/api/state')</script>'''.replace("WORKSPACE", ws)
        with zipfile.ZipFile(source / "design.zip", "w") as z:
            z.writestr("pages/index.html", html)
            z.writestr("pages/next.html", html.replace("href=\"next.html\"", "href=\"index.html\"").replace('id="marker">', 'id="marker">NEXT '))
            z.writestr("assets/theme.css", "#marker { color: rgb(12, 34, 56) }")
            z.writestr("assets/pixel.png", (source / "pixel.png").read_bytes())
        # A small valid PDF with visible text, independent of external generation tools.
        stream = "BT /F1 18 Tf 20 150 Td (Delivered PDF) Tj ET"
        objects = ["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>", "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 240 200] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>", f"<< /Length {len(stream)} >>\nstream\n{stream}\nendstream", "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]
        pdf, offsets = "%PDF-1.4\n", []
        for i, obj in enumerate(objects, 1):
            offsets.append(len(pdf))
            pdf += f"{i} 0 obj\n{obj}\nendobj\n"
        xref = len(pdf)
        pdf += "xref\n0 6\n0000000000 65535 f \n" + "".join(f"{n:010} 00000 n \n" for n in offsets)
        pdf += f"trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        (source / "paper.pdf").write_text(pdf)
else:
    raise ValueError("unknown fixture action")
c.commit()
