-- Digest source writers now maintain inbox membership explicitly in their transactions.
DROP TRIGGER digest_pending_insert;
DROP TRIGGER digest_pending_delete;
DROP TRIGGER digest_receipt_ack;
DROP TRIGGER digest_chan_messages_insert;
DROP TRIGGER digest_chan_messages_update;
DROP TRIGGER digest_chan_messages_delete;
DROP TRIGGER digest_mail_messages_insert;
DROP TRIGGER digest_mail_messages_update;
DROP TRIGGER digest_mail_messages_delete;
