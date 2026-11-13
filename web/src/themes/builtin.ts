/* The built-in choices of Settings → Appearance, written as theme manifests (./manifest.ts) like any other theme.
 * The first of each kind is Steve's default and holds exactly what index.css holds (web/test/themes.test.mjs keeps
 * them equal); the default is never applied as an override, the stylesheet already is it.
 *
 * Accents: the light primary carries white text at WCAG AA (4.5:1) or better and doubles as the link/running blue on
 * every neutral; in dark the primary keeps the same fill (white text again) and the blue and ring are lighter so they
 * read on near-black. Neutrals are Radix slate, gray and sand, mapped step for step onto Steve's slate tokens. */
import type { Manifest } from "./manifest.ts"
import zh from "../i18n/locales/zh-CN/manage.ts"
import en from "../i18n/locales/en/manage.ts"

type Id = keyof typeof zh.look.names
/** a built-in's name in both languages, from the locale files like every other string */
const named = (id: Id) => ({ id, name: { "zh-CN": zh.look.names[id], en: en.look.names[id] } })

const accent = (id: Id, light: string, dark: { ring: string; blue: string }): Manifest => ({
  ...named(id), kind: "accent",
  tokens: {
    light: { primary: light, "primary-foreground": "#ffffff", ring: light, blue: light, "chart-1": light, "sidebar-ring": light },
    dark: { primary: light, "primary-foreground": "#ffffff", ring: dark.ring, blue: dark.blue, "chart-1": dark.ring, "sidebar-ring": dark.ring },
  },
})
export const ACCENTS: Manifest[] = [
  accent("indigo", "#3e63dd", { ring: "#5472e4", blue: "#6683ec" }),
  accent("blue", "#0b6bc0", { ring: "#2a86dc", blue: "#4ba1f5" }),
  accent("violet", "#6e56cf", { ring: "#8270e0", blue: "#9b8af0" }),
  accent("green", "#00786e", { ring: "#14a093", blue: "#34c6b8" }),
  accent("orange", "#b9440c", { ring: "#e0662a", blue: "#f2884a" }),
  accent("rose", "#cb1d63", { ring: "#e23d7d", blue: "#f07aa3" }),
  /* graphite: the primary is the ink itself, so it follows the neutral; links and running keep a quiet steel blue */
  {
    ...named("graphite"), kind: "accent",
    tokens: {
      light: { primary: "var(--foreground)", "primary-foreground": "var(--card)", ring: "var(--muted-foreground)", blue: "#4c5b78", "chart-1": "#4c5b78", "sidebar-ring": "var(--muted-foreground)" },
      dark: { primary: "var(--foreground)", "primary-foreground": "var(--card)", ring: "var(--muted-foreground)", blue: "#9aa8c4", "chart-1": "#9aa8c4", "sidebar-ring": "var(--muted-foreground)" },
    },
  },
]

/** light-mode elevation in the neutral's own ink (dark mode's shadows are black for every neutral) */
const shadows = (ink: string) => ({
  lift: `0 1px 2px rgb(${ink} / 0.04), 0 0 0 1px rgb(${ink} / 0.035)`,
  "lift-hover": `0 2px 8px -2px rgb(${ink} / 0.08), 0 0 0 1px rgb(${ink} / 0.05)`,
  "sh-xs": `0 1px 2px rgb(${ink} / 0.04)`,
  "sh-sm": `0 1px 3px rgb(${ink} / 0.06), 0 1px 2px rgb(${ink} / 0.03)`,
  "sh-md": `0 6px 16px -4px rgb(${ink} / 0.1), 0 0 0 1px rgb(${ink} / 0.05)`,
  "sh-lg": `0 12px 32px -6px rgb(${ink} / 0.14), 0 0 0 1px rgb(${ink} / 0.05)`,
  "sh-xl": `0 28px 64px -12px rgb(${ink} / 0.22), 0 0 0 1px rgb(${ink} / 0.06)`,
})
type Scale = { bg: string; fg: string; muted: string; mutedFg: string; accent: string; border: string; input: string; tertiary: string; chart2: string; chart3: string; other: string; sidebar: string; sidebarBorder: string; canvas: string; code: string; raised: string; card: string; popover: string }
const set = (s: Scale) => ({
  background: s.bg, foreground: s.fg, card: s.card, "card-foreground": s.fg, popover: s.popover, "popover-foreground": s.fg,
  secondary: s.muted, "secondary-foreground": s.fg, muted: s.muted, "muted-foreground": s.mutedFg, accent: s.accent, "accent-foreground": s.fg,
  border: s.border, input: s.input, tertiary: s.tertiary, "chart-2": s.chart2, "chart-3": s.chart3, "chart-4": s.fg, "series-other": s.other,
  sidebar: s.sidebar, "sidebar-foreground": s.fg, "sidebar-border": s.sidebarBorder, "sidebar-accent-foreground": s.fg,
  "canvas-graph": s.canvas, code: s.code, raised: s.raised,
})
const neutral = (id: Id, ink: { wash: string; shadow: string }, light: Scale, dark: Scale): Manifest => ({
  ...named(id), kind: "neutral",
  tokens: {
    light: { ...set(light), "sidebar-primary": light.fg, "sidebar-primary-foreground": "#ffffff", "sidebar-accent": `rgb(${ink.wash} / 0.06)`, ...shadows(ink.shadow) },
    dark: { ...set(dark), "sidebar-primary": dark.fg, "sidebar-primary-foreground": dark.bg, "sidebar-accent": "rgb(255 255 255 / 0.06)" },
  },
})
export const NEUTRALS: Manifest[] = [
  neutral("slate", { wash: "28 32 36", shadow: "16 24 40" },
    { bg: "#fbfbfc", fg: "#1c2024", card: "#ffffff", popover: "#ffffff", muted: "#f0f0f3", mutedFg: "#60646c", accent: "#ececf0", border: "#e6e7eb", input: "#d9d9e0", tertiary: "#80838d", chart2: "#8b8d98", chart3: "#c1c2cb", other: "#b9bbc6", sidebar: "#f6f6f8", sidebarBorder: "#e6e7eb", canvas: "#f9f9fb", code: "#f6f6f8", raised: "#ffffff" },
    { bg: "#111113", fg: "#edeef0", card: "#18191b", popover: "#1d1e21", muted: "#212225", mutedFg: "#a6aab2", accent: "#26282c", border: "#25272b", input: "#34373c", tertiary: "#6f747d", chart2: "#8b8d98", chart3: "#4b5058", other: "#5a6169", sidebar: "#0e0e10", sidebarBorder: "#1d1e21", canvas: "#0e0e10", code: "#161719", raised: "#2b2d31" }),
  neutral("gray", { wash: "32 32 32", shadow: "20 20 20" },
    { bg: "#fbfbfb", fg: "#202020", card: "#ffffff", popover: "#ffffff", muted: "#f0f0f0", mutedFg: "#646464", accent: "#ececec", border: "#e7e7e7", input: "#d9d9d9", tertiary: "#838383", chart2: "#8d8d8d", chart3: "#c2c2c2", other: "#bbbbbb", sidebar: "#f6f6f6", sidebarBorder: "#e7e7e7", canvas: "#f9f9f9", code: "#f6f6f6", raised: "#ffffff" },
    { bg: "#111111", fg: "#eeeeee", card: "#191919", popover: "#1e1e1e", muted: "#222222", mutedFg: "#aaaaaa", accent: "#282828", border: "#272727", input: "#373737", tertiary: "#747474", chart2: "#8e8e8e", chart3: "#505050", other: "#606060", sidebar: "#0e0e0e", sidebarBorder: "#1e1e1e", canvas: "#0e0e0e", code: "#171717", raised: "#2d2d2d" }),
  neutral("sand", { wash: "33 32 28", shadow: "30 26 18" },
    { bg: "#fcfcfb", fg: "#21201c", card: "#ffffff", popover: "#ffffff", muted: "#f1f0ef", mutedFg: "#63635e", accent: "#edeceb", border: "#e8e7e5", input: "#dad9d6", tertiary: "#82827c", chart2: "#8d8d86", chart3: "#c3c2bd", other: "#bcbbb5", sidebar: "#f6f6f5", sidebarBorder: "#e8e7e5", canvas: "#f9f9f8", code: "#f6f6f5", raised: "#ffffff" },
    { bg: "#111110", fg: "#eeeeec", card: "#191918", popover: "#1e1e1d", muted: "#222221", mutedFg: "#aba9a3", accent: "#282826", border: "#272726", input: "#383734", tertiary: "#75736c", chart2: "#8f8d87", chart3: "#51504c", other: "#62605b", sidebar: "#0e0e0d", sidebarBorder: "#1e1e1d", canvas: "#0e0e0d", code: "#171716", raised: "#2d2d2b" }),
]

export const SANS: Manifest[] = [
  { ...named("geist"), kind: "sans", fonts: { sans: '"Geist Variable", -apple-system, BlinkMacSystemFont, "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", "Segoe UI", sans-serif' } },
  { ...named("system"), kind: "sans", fonts: { sans: '-apple-system, BlinkMacSystemFont, system-ui, "Segoe UI", "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", "Noto Sans SC", Roboto, "Helvetica Neue", sans-serif' } },
  { ...named("pingfang"), kind: "sans", fonts: { sans: '"PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", "Noto Sans SC", "Geist Variable", -apple-system, sans-serif' } },
]
export const MONOS: Manifest[] = [
  { ...named("geist-mono"), kind: "mono", fonts: { mono: '"Geist Mono Variable", "SF Mono", ui-monospace, Menlo, Consolas, monospace' } },
  { ...named("system-mono"), kind: "mono", fonts: { mono: 'ui-monospace, "SF Mono", Menlo, Monaco, Consolas, "Liberation Mono", monospace' } },
]
export const RADIUS: Manifest[] = [
  { ...named("standard"), kind: "radius", radius: 1 },
  { ...named("compact"), kind: "radius", radius: 0.67 },
  { ...named("rounded"), kind: "radius", radius: 1.34 },
]

export const BUILTIN: Manifest[] = [...ACCENTS, ...NEUTRALS, ...SANS, ...MONOS, ...RADIUS]
