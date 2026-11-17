import SteveClientKit
import SwiftUI
import UserNotifications

/// Host supplies a binding backed by AttentionMonitor.setPreferences and presents its write failures.
/// Changing a toggle never invokes system authorization. Only the explicit button below does.
struct MacAttentionSettingsView: View {
    @ObservedObject var delivery: MacAttentionDelivery
    @Binding var preferences: AttentionPreferences
    var dedupStatus: AttentionDedupStatus = .ready
    var channels: [String] = []
    @State private var channelID = ""

    var body: some View {
        Form {
            notificationSection
            categorySection
            channelSection
            privacySection
            dedupWarning
        }
        .formStyle(.grouped)
        .task { await delivery.refreshPermission() }
    }

    private var notificationSection: some View {
        Section(L10n.tr("本机提醒", "Notifications on this Mac")) {
            Toggle(L10n.tr("启用系统通知", "Enable system notifications"), isOn: $preferences.enabled)
            Text(L10n.tr("仅监测当前 Hub 的当前工作区。关闭主窗口后继续；退出 App 后停止。",
                         "Monitors only the selected Hub and workspace. Continues with the window closed; stops when the app quits."))
                .font(.caption).foregroundStyle(.secondary)
            Text(permissionDescription)
            if delivery.permission == .notDetermined {
                Button(L10n.tr("允许系统通知…", "Allow notifications…")) {
                    Task { await delivery.requestPermissionFromUserAction(sound: preferences.sound) }
                }
            } else {
                Button(L10n.tr("打开系统通知设置…", "Open notification settings…")) {
                    delivery.openSystemNotificationSettings()
                }
            }
            if delivery.deliveryFailed {
                Text(L10n.tr("系统通知暂不可用；分类提醒和 Dock 标记仍然保留。",
                             "System notification delivery is unavailable; category counts and the Dock indicator remain available."))
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var categorySection: some View {
        Section(L10n.tr("通知类别", "Notification categories")) {
            categoryToggle(.replies, L10n.tr("Steve 回复（消息）", "Steve replies (messages)"))
            categoryToggle(.pending, L10n.tr("待处理（卡片）", "Needs attention (cards)"))
            categoryToggle(.feed, L10n.tr("完成与交付（条目）", "Completions and deliveries (entries)"))
            categoryToggle(.channels, L10n.tr("渠道消息（会话）", "Channel messages (conversations)"))
            Text(L10n.tr("群聊默认遵守服务端提醒设置，可在本机会话设置中明确开启或关闭。quiet 不等于静音。",
                         "Channel banners follow server notification settings unless explicitly overridden locally. Quiet is not mute."))
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    private var channelSection: some View {
        Section(L10n.tr("本机会话设置", "Local channel overrides")) {
            Text(L10n.tr("仅影响此 Mac 的通知，不更改服务端静音或已读状态。", "Affects notifications on this Mac only, not server mute or read state."))
                .font(.caption).foregroundStyle(.secondary)
            ForEach(knownChannels, id: \.self) { id in
                Picker(id, selection: channelPreference(id)) {
                    Text(L10n.tr("遵循服务端", "Follow server")).tag(-1)
                    Text(L10n.tr("本机开启", "On locally")).tag(1)
                    Text(L10n.tr("本机关闭", "Off locally")).tag(0)
                }
            }
            HStack {
                TextField(L10n.tr("会话 ID（不含 chan: 前缀）", "Conversation ID (without chan: prefix)"), text: $channelID)
                Button(L10n.tr("本机开启", "Enable locally")) {
                    let id = trimmedChannelID
                    guard AttentionTarget.validID(id) else { return }
                    preferences.channelOverrides[id] = true
                    channelID = ""
                }.disabled(!AttentionTarget.validID(trimmedChannelID))
            }
        }
    }

    private var privacySection: some View {
        Section(L10n.tr("隐私与声音", "Privacy and sound")) {
            Toggle(L10n.tr("显示类别预览（不包含正文）", "Show category previews (no message bodies)"), isOn: $preferences.privacyPreviews)
            Toggle(L10n.tr("允许通知声音（遵守系统设置）", "Allow notification sound (subject to system settings)"), isOn: $preferences.sound)
        }
    }

    @ViewBuilder private var dedupWarning: some View {
        if dedupStatus != .ready {
            Text(dedupStatus == .capacityReached
                 ? L10n.tr("去重记录已达本机上限；为避免重复，当前安装周期暂停横幅。分类计数继续更新。",
                           "The local deduplication limit was reached. Banners are paused for this Hub epoch to avoid repeats; category counts continue updating.")
                 : L10n.tr("本机去重存储不可用；暂停横幅，分类计数继续更新。",
                           "Local deduplication storage is unavailable. Banners are paused; category counts continue updating."))
                .foregroundStyle(.secondary)
        }
    }

    private var knownChannels: [String] {
        Array(Set(channels + Array(preferences.channelOverrides.keys))).sorted()
    }
    private var trimmedChannelID: String { channelID.trimmingCharacters(in: .whitespacesAndNewlines) }
    private func channelPreference(_ id: String) -> Binding<Int> {
        Binding(get: { preferences.channelOverrides[id].map { $0 ? 1 : 0 } ?? -1 }, set: { value in
            if value == -1 { preferences.channelOverrides.removeValue(forKey: id) }
            else { preferences.channelOverrides[id] = value == 1 }
        })
    }
    private func categoryToggle(_ kind: AttentionKind, _ title: String) -> some View {
        Toggle(title, isOn: Binding(get: { preferences.enabledCategories.contains(kind) }, set: { enabled in
            if enabled { preferences.enabledCategories.insert(kind) }
            else { preferences.enabledCategories.remove(kind) }
        }))
    }
    private var permissionDescription: String {
        switch delivery.permission {
        case .authorized, .provisional: return L10n.tr("系统通知已允许", "System notifications allowed")
        case .denied: return L10n.tr("系统通知已拒绝；可在系统设置中开启", "System notifications denied; enable them in System Settings")
        default: return L10n.tr("尚未请求系统通知权限", "System notification permission has not been requested")
        }
    }
}
