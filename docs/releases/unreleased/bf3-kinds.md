Needs a minor release: category initialization now refuses legacy tags whose reserved category name is used in another tag category.

Before upgrading, rename any non-category tag named bug, feat, doc, refactor, ops or chore. Initialization reports a conflict instead of changing that tag's category and silently adding another category to its items. A refused initialization leaves the existing tags and items unchanged.

Workflow child creation validates inherited tags through the same single-category check as ordinary task creation. An invalid parent category set must be corrected before dispatch; refusal does not create a child or consume its item number. No historical category backfill is performed.
