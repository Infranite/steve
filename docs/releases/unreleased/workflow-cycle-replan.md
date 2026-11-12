When an item starts a new execution cycle, its paused work graph can now be replanned after old executions have stopped. Prior work and review results remain in history with their original verdicts, while the new cycle starts with fresh work units and acceptance gates.

Nodes with a long event-stream history can reconnect again: the handshake stays bounded, and stream positions are read in pages after the link is established. Workflow activity no longer turns "unconfirmed" merely because the Node's original connection time is older than ten minutes.
