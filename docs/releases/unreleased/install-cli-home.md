Needs a minor release: the installation CLI honors STEVE_HOME and service actions require a matching registration

Before upgrading, check that each installation has `services/hub.json` or
`services/node.json` in its home. If that registration is missing, rerun
`steve setup hub` or `steve setup node` for the original home and service label
before using start, stop, restart or uninstall. Use `--home` and `--label` for
custom installations. There is no fallback for installations without a registration.

The installation CLI now selects its home from `--home`, then `STEVE_HOME`, then
`$HOME/.steve` (Hub) or `$HOME/.steve-node` (Node), including setup and reconfiguration.
Start, stop, restart and uninstall require that home's own service registration
with the matching component and label before contacting launchd or systemd.
`STEVE_HOME` is shared by both roles; select the Node's home explicitly when needed.
