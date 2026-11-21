Schedule startup failures now store a stable error code and parameters, so changing language does not split repeated failures or reset their budget. Run history and the latest-run summary are translated in the reader's current language.

Startup diagnostics distinguish damage that still blocks the current configuration, historical damage after a configuration save, and records whose source cannot be identified. Historical records no longer keep telling you to edit an already changed configuration.

On startup, old digest startup-failure markers that stored translated text are deleted together with dependent steps, waits and links. Independent items and child runs are kept; references to deleted runs are cleared. Old failure counts are not converted or backfilled, so affected configurations start a new bounded retry budget. Ordinary run history and successful startup anchors are kept.
