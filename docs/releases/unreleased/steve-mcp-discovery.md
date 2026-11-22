Needs a minor release: the Node protocol is now 11; upgrade the Hub and all Nodes together.

Agents get Steve's tools again from the first turn: a session lists them as soon as its harness connects, without waiting for a running attempt (calls still need one). An attempt whose harness never listed them is now noted once in the session's timeline and counted in `steve_session_steve_tools_missing_total`.
