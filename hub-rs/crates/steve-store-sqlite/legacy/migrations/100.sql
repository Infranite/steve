-- 100: the calendar read cache (steve-calendar). The provider stays the source of truth; every row here can be
-- rebuilt by a sync. A calendar belongs to the connection it came from and is read only by the user who owns
-- that connection (`owner_user`); `party_id` is the person whose calendar it is (the owner's own, or someone
-- else's whose free/busy or shared details the source shows). Disconnecting drops the calendars and their events.
CREATE TABLE cal_calendars (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
  owner_user TEXT NOT NULL,
  ext_id TEXT NOT NULL,
  name TEXT NOT NULL,
  color TEXT,
  kind TEXT NOT NULL DEFAULT 'primary',        -- primary|shared|team|subscribed|steve
  role TEXT NOT NULL DEFAULT 'reader',         -- what the source lets the owner do: free_busy|reader|writer|owner
  share TEXT NOT NULL DEFAULT 'private',       -- who else sees it at the source: private|busy|details
  party_id TEXT,                               -- whose calendar it is
  party_set INTEGER NOT NULL DEFAULT 0,        -- 1: the owner chose the person, a sync keeps it
  detail TEXT NOT NULL DEFAULT 'full',         -- what Steve keeps: full | busy (times only)
  enabled INTEGER NOT NULL DEFAULT 1,
  tz TEXT,
  sync_cursor TEXT,
  synced_at INTEGER,                           -- the last sync that succeeded
  full_at INTEGER,                             -- the last whole-window pull that succeeded; a day later the next sync pulls it again
  sync_state TEXT NOT NULL DEFAULT 'new',      -- new|ok|error
  sync_error TEXT,
  gone INTEGER NOT NULL DEFAULT 0,             -- the source no longer lists it
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  UNIQUE (connection_id, ext_id));
CREATE INDEX cal_calendars_owner ON cal_calendars(workspace_id, owner_user);

-- One occurrence of an event. `id` is a hash of (calendar, ext_id, original_start): a resync keeps it, so links
-- to it stay. All-day events keep only local dates (`start_date`, `end_date` exclusive); `start_ms`/`end_ms` of
-- one are the dates' bounds in UTC, for ordering only. `version` goes up only when something that matters to
-- the people in it changed (time, place, attendees, status, my reply), never for a description.
CREATE TABLE cal_events (
  id TEXT PRIMARY KEY,
  calendar_id TEXT NOT NULL REFERENCES cal_calendars(id) ON DELETE CASCADE,
  workspace_id TEXT NOT NULL,
  owner_user TEXT NOT NULL,
  meeting_id TEXT,
  ext_id TEXT NOT NULL,
  series_ext_id TEXT,
  original_start TEXT,
  ical_uid TEXT,
  sequence INTEGER,
  etag TEXT,
  version INTEGER NOT NULL DEFAULT 1,
  fingerprint TEXT NOT NULL,
  title TEXT,
  location TEXT,
  meeting_url TEXT,
  excerpt TEXT,
  start_ms INTEGER NOT NULL,
  end_ms INTEGER NOT NULL,
  tz TEXT,
  all_day INTEGER NOT NULL DEFAULT 0,
  start_date TEXT,
  end_date TEXT,
  status TEXT NOT NULL DEFAULT 'confirmed',    -- confirmed|tentative|cancelled
  my_rsvp TEXT,                                -- accepted|declined|tentative|needs_action
  organizer_party TEXT,
  organizer_email TEXT,
  organizer_name TEXT,
  i_am_organizer INTEGER NOT NULL DEFAULT 0,
  can_modify INTEGER NOT NULL DEFAULT 0,
  busy_only INTEGER NOT NULL DEFAULT 0,        -- only the time is known (free/busy)
  origin TEXT NOT NULL DEFAULT 'source',
  deleted_ms INTEGER,
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL);
CREATE INDEX cal_events_owner_time ON cal_events(workspace_id, owner_user, start_ms);
CREATE INDEX cal_events_calendar ON cal_events(calendar_id, start_ms);
CREATE INDEX cal_events_meeting ON cal_events(meeting_id);

CREATE TABLE cal_attendees (
  event_id TEXT NOT NULL REFERENCES cal_events(id) ON DELETE CASCADE,
  ord INTEGER NOT NULL,
  party_id TEXT,
  email TEXT,
  name TEXT,
  ext_ref TEXT,
  rsvp TEXT,
  organizer INTEGER NOT NULL DEFAULT 0,
  optional INTEGER NOT NULL DEFAULT 0,
  is_self INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (event_id, ord));
CREATE INDEX cal_attendees_party ON cal_attendees(party_id);

-- The same meeting seen in several calendars of one person (an invitation in two sources): what later alarms,
-- preparation and links hang on. `primary_event_id` is the occurrence shown.
CREATE TABLE cal_meetings (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  owner_user TEXT NOT NULL,
  primary_event_id TEXT NOT NULL,
  match_key TEXT NOT NULL,
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL);
CREATE INDEX cal_meetings_owner ON cal_meetings(workspace_id, owner_user);

-- Two occurrences the owner said are not the same meeting: never merged again.
CREATE TABLE cal_not_same (
  workspace_id TEXT NOT NULL,
  owner_user TEXT NOT NULL,
  a TEXT NOT NULL,
  b TEXT NOT NULL,
  created INTEGER NOT NULL,
  PRIMARY KEY (owner_user, a, b));

-- An .ics file uploaded to an ICS connection (instead of a subscription address).
CREATE TABLE cal_ics_files (
  connection_id TEXT PRIMARY KEY REFERENCES connections(id) ON DELETE CASCADE,
  workspace_id TEXT NOT NULL,
  name TEXT NOT NULL,
  body TEXT NOT NULL,
  uploaded INTEGER NOT NULL);
