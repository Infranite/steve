"""Synthetic records and package for markdown-images.browser.mjs's own empty installation."""
import json
import pathlib
import sqlite3
import sys
import time
import zipfile

home, action = pathlib.Path(sys.argv[1]), sys.argv[2]
assert (home / ".image-browser-test").is_file(), "not an image browser test installation"
if action == "package":
    with zipfile.ZipFile(home / "images.zip", "w") as z:
        z.writestr("docs/README.md", "# Image package\n\n![交付包相对图片](../assets/test.png)\n\n[包内说明](../about.md)\n")
        z.writestr("about.md", "Synthetic package information\n")
        z.write(home / "test-image.png", "assets/test.png")
elif action == "seed":
    data = json.load(sys.stdin)
    c = sqlite3.connect(home / "data/steve.sqlite3", timeout=20)
    now = int(time.time() * 1000)
    user = c.execute("SELECT id FROM users WHERE subject='dev:local'").fetchone()[0]
    c.execute("INSERT INTO workspaces(id,name,created) VALUES('image-other','Image other workspace',?)", (now,))
    c.execute("INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES('image-other',?,'owner',?)", (user, now))
    c.execute("INSERT INTO messages(id,workspace_id,space,author,text,created,meta) VALUES('image-web-test','default','steve','steve',?,?, '{}')", (data["markdown"], now))
    c.commit()
else:
    raise ValueError("unknown fixture action")
