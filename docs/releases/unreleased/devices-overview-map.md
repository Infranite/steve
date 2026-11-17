**Devices → Overview** starts with what wants you, one line each with its next step:
- a Node to confirm (**Confirm** opens its fingerprint check);
- an upgrade that failed and went back;
- an upgrade under way;
- a Node behind the Hub (with **Upgrade**);
- a Node offline, with the first words of its last error;
- sessions waiting for your answer.

Under it are four numbers: sessions running against the total limit, Nodes online, Nodes on the Hub's version and clients online.

Then a **connection map**:
- The clients on the left, the Hub and its own Node in one frame for the computer they share, and the remote Nodes on the right.
- Lines from the Hub to a Node show its state by color and by line style. They get thicker with its sessions and move while sessions run, and they are labelled with the route (this machine, LAN, tailnet, tailcat, HTTPS) once the Hub reports it.
- Pointing at a card or focusing it shows its facts and lights its own line. Arrow keys move within a column, and a click opens the Hub, the Node or the client's row.
- Seven or more Nodes take one line each and more than twelve fold into "N more". With no other computer, a card explains when to add one.
- On a phone the map is a tree. **Show clients** turns the client column off and is remembered.

**Add device** is now one dialog for both kinds, step by step. It replaces the separate Node pairing dialog, the client QR dialog and the tailcat section.
- For a **Node**, choose how it reaches the Hub: the same network or HTTPS, tailcat (the owner can turn it on right there) or a remote install. Then copy the command and the pairing code, wait for it to connect and check its fingerprint.
- The fingerprint is checked in full. Paste what the Node printed, or type its eight groups; each group is checked as you go, and the Node can be added only when all eight match. **They differ, reject** unpairs it and makes a new code. A check of two random groups was considered and rejected as weaker: someone who knows a Node's key can make another key that matches two given groups.
- For a **client**, it shows the QR code as before.

`steve-node pair`, `status` and `rotate` print the fingerprint in groups of four.
