export default {
  "upgrade": {
    "title": "Version",
    "policyTitle": "Upgrade policy",
    "followDefault": "Hub default ({{policy}})",
    "policy": {
      "auto": "Upgrade with the Hub",
      "notify": "Only show it",
      "off": "Off"
    },
    "reason": {
      "unknown": "Its build is not known yet (offline, or not asked yet)",
      "hub_dev_build": "The Hub is a development build: nothing to upgrade to"
    },
    "phase": {
      "preparing": "Preparing",
      "staging": "Downloading",
      "staged": "Downloaded",
      "draining": "Waiting for running work to finish",
      "switching": "Switching",
      "succeeded": "Upgraded",
      "failed": "Failed, nothing switched",
      "rolled_back": "Rolled back",
      "abandoned": "Given up",
      "unknown": "Outcome unknown"
    },
    "force": "Upgrade now, cutting running work",
    "started": "Upgrade started"
  },
  "state": {
    "starting": "Starting",
    "error": "Error"
  }
}
