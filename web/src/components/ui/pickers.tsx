import { useT, lang } from "@/i18n"
import { DatePicker } from "@/components/affect/date-picker-clean"
import { TimePicker } from "@/components/affect/time-picker-clean"
import { NumberInput, type NumberInputProps } from "@/components/affect/number-input-clean"

/* affectUI's Date Picker, Time Picker and Number Input in Steve's terms: a day as "YYYY-MM-DD" and a time as "HH:mm"
 * (the strings the Hub stores), the page's language for month names, weekdays, the 24-hour clock and the controls'
 * names. */

const pad = (n: number) => String(n).padStart(2, "0")
const toDay = (s?: string | null) => (s && /^\d{4}-\d{2}-\d{2}$/.test(s) ? new Date(s + "T00:00:00") : null)
const fromDay = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`

export function DateField({ value, onChange, min, max, size = "md", className, placeholder, ...rest }: { value: string | null; onChange: (day: string) => void; min?: string; max?: string; size?: "sm" | "md" | "lg"; className?: string; placeholder?: string; "aria-label"?: string; id?: string; [data: `data-${string}`]: string | undefined }) {
  const t = useT()
  return (
    <DatePicker {...rest} value={toDay(value)} onChange={(d) => onChange(fromDay(d))} min={toDay(min) ?? undefined} max={toDay(max) ?? undefined} locale={lang()} size={size}
      className={className} placeholder={placeholder ?? ""} labels={{ prev: t("common:picker.prevMonth"), next: t("common:picker.nextMonth") }} />
  )
}

export function TimeField({ value, onChange, step = 30, size = "md", className, ...rest }: { value: string | null; onChange: (time: string | null) => void; step?: number; size?: "sm" | "md" | "lg"; className?: string; "aria-label"?: string; id?: string }) {
  const t = useT()
  return (
    <TimePicker {...rest} value={value} onChange={onChange} step={step} hourCycle={24} locale={lang()} size={size} className={className}
      labels={{ show: t("common:picker.showTimes"), hide: t("common:picker.hideTimes"), times: t("common:picker.times") }} />
  )
}

export function NumberField(props: Omit<NumberInputProps, "labels" | "locale">) {
  const t = useT()
  return <NumberInput locale={lang()} {...props} labels={{ increase: t("common:picker.increase"), decrease: t("common:picker.decrease") }} />
}
