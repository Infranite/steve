Needs a minor release: the manual item-category backfill and category-review APIs have been removed.

The Hub no longer runs the one-time item-category backfill at startup. The historical-item check and review controls have been removed from tag management, along with POST /tags/kinds/backfill and GET/POST /tags/kinds/review. New task items still receive exactly one category through the normal write paths. No database migration or historical data conversion is performed.
