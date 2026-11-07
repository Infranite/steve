macOS 系统沙箱里的运行不能启动 setuid 程序，例如 `ps`、`top`、`sudo`。查看进程可以使用 `pgrep` 等非 setuid 工具；该限制不适用于仍使用自身沙箱的 harness。
