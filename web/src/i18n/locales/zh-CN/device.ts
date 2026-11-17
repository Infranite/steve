export default {
  "upgrade": {
    "title": "版本",
    "policyTitle": "升级策略",
    "followDefault": "跟随 Hub 默认（{{policy}}）",
    "policy": {
      "auto": "跟随 Hub 自动升级",
      "notify": "仅提示",
      "off": "关闭"
    },
    "reason": {
      "unknown": "暂不知道它的版本（离线或尚未询问）",
      "hub_dev_build": "Hub 是开发构建，没有可升级到的版本"
    },
    "phase": {
      "preparing": "准备中",
      "staging": "下载中",
      "staged": "已下载",
      "draining": "等运行中的工作结束",
      "switching": "切换中",
      "succeeded": "已升级",
      "failed": "失败，未切换",
      "rolled_back": "已回滚",
      "abandoned": "已放弃",
      "unknown": "结果未知"
    },
    "force": "立即升级（中断运行中的工作）",
    "started": "已开始升级"
  },
  "state": {
    "starting": "启动中",
    "error": "错误"
  }
}
