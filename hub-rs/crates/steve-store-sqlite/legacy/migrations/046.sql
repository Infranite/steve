-- 46: Steve's own project, as the seed made it, released by running $STEVE_HOME/current/ops/release.sh on
-- this machine. Its releases go through the merge lane instead (the repository's ops/cut-release.py tags a
-- tested main): no command of its own, done once merged and tagged. A rule of done already chosen is kept;
-- a command edited by hand (anything but the seeded path) is left alone.
UPDATE projects
SET release = json_set(release, '$.cmd', '', '$.done_when', coalesce(json_extract(release, '$.done_when'), 'tag'))
WHERE json_valid(release)
  AND json_extract(release, '$.kind') = 'script'
  AND json_extract(release, '$.cmd') LIKE '%/current/ops/release.sh';
