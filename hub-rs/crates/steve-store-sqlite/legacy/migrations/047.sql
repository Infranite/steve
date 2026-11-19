-- 发送前的撤销窗口（秒），每个渠道账号一个；NULL：按身份的默认值（机器人 3 秒，你本人 10 秒）。
ALTER TABLE channel_settings ADD COLUMN send_delay_s INTEGER CHECK(send_delay_s IS NULL OR send_delay_s BETWEEN 0 AND 60);
