The Node runtime tests wait for committed turn starts, avoiding intermittent timeouts when a session reopens. Nextest now fails local and CI runs when a test leaves its output pipes open.
