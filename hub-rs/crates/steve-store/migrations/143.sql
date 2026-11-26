-- Rebind work.tidy from work.item.done to work.item.lifecycle so every close kind
-- (completed, cancelled, failed) runs deterministic post-close session stop and worktree cleanup.
-- Historical already-closed items are cleaned by the one-time idempotent backfill task.
UPDATE outbox_consumers
   SET topic = 'work.item.lifecycle',
       since_seq = COALESCE(
           (SELECT MAX(event_seq) FROM outbox_receipts WHERE consumer_id = 'work.tidy'),
           since_seq
       )
 WHERE id = 'work.tidy'
   AND topic = 'work.item.done';
DELETE FROM outbox_leases WHERE consumer_id = 'work.tidy';
DELETE FROM outbox_receipts WHERE consumer_id = 'work.tidy';
