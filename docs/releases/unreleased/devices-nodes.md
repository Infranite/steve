**Devices → Nodes** shows one card per Node, the Hub's own first. Each card shows:
- the computer it runs on;
- its status, and a plain line when something needs doing: an upgrade that failed and went back, a Node too old to upgrade from here, a Node behind the Hub, offline with its last error, or a fingerprint to confirm;
- CPU load, memory, the round trip to the Hub and its sessions, each with its last hour;
- what its agents are doing.

A Node's page is the same for the Hub's own Node and a paired one.
- **Now**: its figures, or its last data with a note while it is offline. Then its version, its running sessions (those waiting for your answer first, answerable from there) and the last 24 hours as charts.
- **Settings**: whether it takes new work, how many sessions it runs at once, its tags, personal agent config, its harnesses (the Hub's own Node), its identity and its project folders.

The **Version** card follows a Hub-managed upgrade step by step and quotes the Hub's real waits:
- what upgrading does, with **Upgrade to …** and **Upgrade now**;
- while it waits for running sessions, how many are left, with **Switch now**: that upgrade stops waiting and switches at once (the stopped sessions continue after the Node restarts);
- after a failure, the error, **Copy error** and **Try again**;
- for a Node that is upgraded on its own computer, the command to run there.

**Personal agent config** is now one default for the whole Hub, under **Devices → Hub → Defaults for every Node**, next to the default upgrade policy, because a Node serves every workspace. Each Node can still follow it or choose for itself. Upgrading starts the Hub's default as off if any workspace had turned the old workspace setting off; **Settings → Items** now links to the new place. API: `GET`/`PUT /api/node-context` (instance owner), `GET /api/nodes/upgrade-default` also returns `drain_secs` and `health_secs`, and `POST /api/nodes/{id}/upgrade/switch` (instance owner) makes the Node's upgrade under way switch now; it never starts another upgrade.
