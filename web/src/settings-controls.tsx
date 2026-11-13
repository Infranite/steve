/* The controls of the settings pages: affectUI's Select and Switch (web/src/components/affect), wrapped once so every
 * pane passes Steve's options and keeps the test hooks (`data-select`, on the combobox itself) the rest of the app uses. Below 640px a select in a
 * settings row drops under its label (affectui-theme.css, `data-set-select`). */
import { Select } from "@/components/affect/select-clean"
import { Switch } from "@/components/affect/switch-clean"
import { cn } from "cn"

/** a choice in a settings row. `wide` for a field that fills its column (a sheet), otherwise a fixed width so the
 *  selects down one card line up on their right edge */
export function SetSelect({ value, options, onChange, testId, label, wide, disabled, className }: { value: string; options: [string, string][] | { v: string; l: string }[]; onChange: (v: string) => void; testId?: string; label?: string; wide?: boolean; disabled?: boolean; className?: string }) {
  const os = options.map((o) => (Array.isArray(o) ? { value: o[0], label: o[1] } : { value: o.v, label: o.l }))
  return (
    <div data-set-select={wide ? undefined : ""} className={cn("min-w-0", wide ? "w-full" : "w-40 max-w-full", className)}>
      <Select size={wide ? "md" : "sm"} value={value} options={os} onChange={(v) => onChange(v)} aria-label={label} disabled={disabled} data-select={testId} />
    </div>
  )
}

/** on or off, taking effect at once; the row around it carries the words, so the switch only needs a name */
export function SetSwitch({ checked, onChange, label, testId }: { checked: boolean; onChange: (v: boolean) => void; label: string; testId?: string }) {
  return <Switch checked={checked} onChange={(e) => onChange(e.currentTarget.checked)} aria-label={label} data-switch={testId} />
}

