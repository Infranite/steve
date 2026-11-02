# Initech quarterly business review

Attendees: Michael Bolton (Initech), Samir N. (Initech), me.

## Highlights

- Initech's usage grew 35% quarter over quarter, mostly from the reporting module.
- They want single sign-on with Okta before the end of the year. We support OIDC, so this is configuration work on their side plus a test tenant from us.

## Concerns

- Report exports time out for ranges longer than a year. Root cause is the CSV export running synchronously; the fix (background export with a download link) shipped in ST-15.
- They asked for a data processing agreement (DPA) update covering EU data residency.

## Action items

- Send the updated DPA draft to Michael by November 8.
- Set up an OIDC test tenant for Okta integration.
- Schedule the next QBR for late January.
