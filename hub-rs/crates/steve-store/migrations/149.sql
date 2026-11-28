-- 149: remove retired agent-robot-avatar skin field from bots.avatar JSON (#645).
UPDATE bots
   SET avatar = json_remove(avatar, '$.skin')
 WHERE avatar IS NOT NULL
   AND json_valid(avatar)
   AND json_type(avatar, '$.skin') IS NOT NULL;
