Needs a minor release: initialization no longer backfills historical delivery tags and can recreate missing built-in delivery names.

Category initialization no longer moves nonstandard tags from the type category into the topic category, and no longer infers delivery tags from historical item titles, goals or artifacts. Existing item tags are left as they are; there is no historical backfill or migration of the old checkpoint setting.

Missing built-in delivery tags are now seeded on every initialization. Deleting or renaming one can recreate its standard name on the next initialization. If that name already belongs to a tag in another category, the existing tag is preserved and that delivery seed is skipped; it does not block startup or task creation. New tasks and new outputs still receive available delivery tags, and skip unavailable ones without attaching the ordinary tag of the same name.
