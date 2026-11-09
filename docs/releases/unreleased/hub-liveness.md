用 `steve-hub --supervise` 运行的托管安装里，supervisor 每 30 秒探测一次 Hub 的 `/healthz`。Hub 启动后回应过、之后连续 4 次没有回应，就视为卡死：supervisor 停掉它并退出，由 launchd/systemd 重新拉起，和 Hub 自己退出时的处理一样。升级进行中不计入。
