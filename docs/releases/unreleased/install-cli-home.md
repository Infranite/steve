Needs a minor release: the installation CLI honors STEVE_HOME and service actions require a matching registration

Before upgrading, check that each installation has `services/hub.json` or
`services/node.json` in its home. If that registration is missing, rerun
`steve setup hub` or `steve setup node` for the original home and service label
before using start, stop, restart or uninstall. Use `--home` and `--label` for
custom installations. There is no fallback for installations without a registration.

Previously, the installation CLI ignored `STEVE_HOME` when `--home` was omitted.
With the real account's `HOME`, `steve hub stop` selected `$HOME/.steve` and
stopped the Hub installed there, including a production Hub, even when
`STEVE_HOME` pointed to a different installation. On macOS, launchd's `bootout`
targets the account's GUI domain and service label, so changing the intended installation home alone did not isolate it.

The installation CLI now selects its home from `--home`, then `STEVE_HOME`, then
`$HOME/.steve` (Hub) or `$HOME/.steve-node` (Node), including setup and reconfiguration.
Start, stop, restart and uninstall require that home's own service registration
with the matching component and label before contacting launchd or systemd.
`STEVE_HOME` is shared by both roles; select the Node's home explicitly when needed.
An interrupted setup that has not registered a service can still be set aside
with `steve setup --fresh`; it skips service operations and preserves the old home.
