-- 94: what each paired Node's machine is and how its latest link reached the Hub, kept for when it
-- is away (the devices page). `host`: the Node's own account of its machine (`node.host`, JSON).
-- `link`: the route, transport and start of its latest link, as the Hub saw it arrive (JSON).
ALTER TABLE nodes ADD COLUMN host TEXT;
ALTER TABLE nodes ADD COLUMN link TEXT;
