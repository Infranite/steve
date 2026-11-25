-- Clear cached harness update state written before typed update reasons (#558).
DELETE FROM settings WHERE key LIKE 'harness.updates.%';
