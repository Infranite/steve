Needs a minor release: the `GET /digest/conversations/{conv}` API response no longer includes `cursor.since`; clients that read this field must stop using it.

Digest intake now reads only new or pending messages instead of scanning all message history every minute, while still processing late arrivals and newly sent outbound messages exactly once.
