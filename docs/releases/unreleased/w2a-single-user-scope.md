Needs a minor release: custom clients using user-scoped device notifications must treat node and client events as Hub-wide notifications.

On a single-user Hub, node and client SSE notifications now reach all connected workspaces. The shared Scope type now contains only Ws and Hub; the User variant is removed. Event payloads still include the user field. The web client uses the updated generated types. Signing a client out still closes only that client's streams, and client presence notifications still leave the state cache valid.
