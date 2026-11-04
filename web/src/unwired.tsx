/* Modules that are designed but not wired to the Hub yet. They keep their place in the navigation and say plainly
 * what they will do and when — no sample data, nothing that looks like it works. */
import { BookOpenIcon, ContactRoundIcon, KeyRoundIcon, MessagesSquareIcon, PlugIcon, RadioIcon, UserRoundIcon, WorkflowIcon, LockIcon, MailIcon, ChartNoAxesColumnIcon, type LucideIcon } from "lucide-react"
import { Page, SectionLabel, Group, Row } from "@/ui"

type Mod = { title: string; sub: string; icon: LucideIcon; phase: string; will: [string, string][]; now?: string }
export const UNWIRED: Record<string, Mod> = {
  contacts: { title: "联系人", sub: "你打交道的人、组织和团队", icon: ContactRoundIcon, phase: "P1 事项主线之后",
    will: [["联系人与组织", "关系、任职、各系统里的账号，往来时间线，相关事项和记忆"], ["团队", "从办公 IM 同步组织架构，也可以自己组虚拟团队"], ["合并提议", "同一个人在不同系统里的账号，Steve 提议合并，你确认"], ["@联系人", "在对话里 @ 一个人，把他关联到事项"]] },
  signals: { title: "信号", sub: "邮件、IM、日程等外部信息，以及 Bot 怎么处理的", icon: RadioIcon, phase: "P3 沟通与知识",
    will: [["分诊", "每条外部信息按流程处理：需要你、建事项、进汇报或忽略"], ["纠正", "处理得不对时一键改成“以后提醒我”或“以后忽略”，改进对应流程"], ["原文保留", "原文按期限保留，过期后只留处理结果和摘要"]],
    now: "先要在“接入”里接入外部服务（IM、邮件、日历）。接入之前这里不会有内容。" },
  auto: { title: "自动化", sub: "流程、定时任务和事件源", icon: WorkflowIcon, phase: "P4 自动化与扩展",
    will: [["定时任务", "每天、每周或按时间点交给某个 Bot 做一件事"], ["流程", "收到某类事件时怎么处理：过滤、查表、交给 Bot、等你批准、执行动作"], ["事件源", "邮件、IM、日历、GitHub、Webhook 的接收范围在各自的账号里设置"]] },
  accounts: { title: "账号", sub: "我的账号、接入 Steve 的连接", icon: KeyRoundIcon, phase: "P3 沟通与知识",
    will: [["账号", "按系统分组：你的账号和本公司的应用机器人账号"], ["接入 Steve", "授权后显示能做什么、绑定的对外身份、所在的群"], ["匹配规则", "哪些账号属于哪个联系人"]] },
  vault: { title: "保险箱", sub: "密码、令牌、银行卡这类机密", icon: LockIcon, phase: "P4 自动化与扩展",
    will: [["条目", "按模板保存登录、API 凭据、银行卡等；机密字段只显示掩码，值不进模型"], ["使用审批", "Bot 按条目和用途申请使用，默认每次都要你批准；高密条目不允许授权"], ["使用记录", "谁在什么时候、为了什么用过；外部保险库只留接口"]] },
  personas: { title: "对外身份", sub: "Bot 在各个渠道里以什么身份出现", icon: UserRoundIcon, phase: "P3 沟通与知识",
    will: [["身份", "名字、头像、自我介绍、透露设置、签名"], ["绑定", "绑定到应用机器人账号，或某个群里单独换身份"], ["披露检查", "回复前按听众检查能说到什么程度"]] },
  mail: { title: "邮件", sub: "邮箱的往来、分诊和回复草稿", icon: MailIcon, phase: "Rust Hub 上线后", will: [["往来", "按需要处理、收件箱、等对方、已发送、草稿查看每串往来"], ["起草回复", "邮件 Bot 起草，你改过、批准后才发出，10 秒内可以撤销"], ["分诊", "通知类邮件进汇报，需要回复的列出来"]], now: "这个 Hub 还没有邮件模块，或者还不能列出邮件往来。" },
  github: { title: "GitHub", sub: "仓库、PR 和检查", icon: PlugIcon, phase: "Rust Hub 上线后", will: [["接入", "建一个 GitHub App（推荐），或用个人令牌"], ["仓库与项目", "仓库关联到项目，事项分支的 PR 和检查自动跟踪"], ["验收与合并", "验收后问你要不要开 PR，检查全绿后问你要不要合并"]], now: "这个 Hub 还没有 GitHub 模块。" },
  knowledge: { title: "知识", sub: "Steve 记得的事和能查到的资料", icon: BookOpenIcon, phase: "P3 沟通与知识", will: [["搜索", "在长期记忆、知识条目、笔记、文件夹、事项记录和会话记录里找"], ["长期记忆", "你明确说过以后都适用的偏好和规则"], ["知识条目", "确认过、别的事也用得上的事实和做法"], ["来源", "笔记、文件和本机文件夹"], ["待确认", "Steve 发现值得长期保留的条目，你确认后才用"]], now: "现在的 Hub 还没有知识库接口，换成新版 Hub 后这里就能用。" },
  stats: { title: "统计", sub: "token、缓存命中率、估算花费和完成效率", icon: ChartNoAxesColumnIcon, phase: "Rust Hub 上线后", will: [["用量与花费", "token 进出、缓存命中率，按价格表估算的费用（标“估”）"], ["效率", "完成事项数、完成一件事项的平均时间、首次通过率、等你的时间"], ["下钻", "每件事项的回合、决定、交付和发布，以及它花的 tokens"]], now: "这个 Hub 还没有统计接口。" },
  integrations: { title: "接入", sub: "Steve 能连接的外部服务，以及每个连接能做什么", icon: PlugIcon, phase: "Rust Hub 上线后",
    will: [["服务", "IM、文档、日历、任务、邮件、代码仓库、通讯录；每个服务按步骤接入"], ["能力", "每个连接列出已开启、缺权限和规划中的能力"], ["身份", "机器人和你本人分开接入、分开授权"]], now: "这个 Hub 还没有接入模块。" },
  channels: { title: "渠道", sub: "各个接入里的群和私聊", icon: MessagesSquareIcon, phase: "Rust Hub 上线后",
    will: [["群和私聊", "所有能收发消息的连接里的会话，按来源和账号筛选"], ["每个群的设置", "说话的身份、什么时候响应、静默、内容能传到哪里"], ["对外身份", "Bot 在群里出面用的名字、自我介绍和口吻"]], now: "这个 Hub 还没有渠道模块。" },
  plugins: { title: "插件", sub: "给 Steve 增加事件源、动作和能力", icon: PlugIcon, phase: "P4 自动化与扩展",
    will: [["已安装", "内置、官方和本地开发的插件；停用后它提供的能力不可用"], ["权限", "安装前列出它要的权限"], ["开发", "从本地目录安装，改动后自动重载"]] },
}

export function Unwired({ id }: { id: keyof typeof UNWIRED | string }) {
  const m = UNWIRED[id] ?? UNWIRED.contacts, I = m.icon
  return (
    <Page title={m.title} sub={m.sub} max={640}>
      <div className="flex flex-col gap-8" data-unwired={id}>
        <div className="flex items-start gap-3 rounded-xl border px-4 py-3.5">
          <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground"><I className="size-[18px]" strokeWidth={1.5} /></span>
          <div className="min-w-0"><div className="type-headline">还没有接入</div>
            <div className="mt-0.5 type-callout text-muted-foreground">{m.now ?? "这个模块的设计已经定了，后端还没有做。现在这里不会有数据，也不能新建。"} 计划在 {m.phase}。</div></div>
        </div>
        <section><SectionLabel>接入后可以</SectionLabel>
          <Group>{m.will.map(([t, d]) => <Row key={t} title={t} sub={d} />)}</Group></section>
      </div>
    </Page>
  )
}
