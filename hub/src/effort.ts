/** ACP adapters identify reasoning controls either by category or by their stable config id. */
export function effortOption(options: any[] = []): any | undefined {
  return options.find((o) => o.category === "thought_level" || ["thought_level", "reasoning_effort", "effort"].includes(o.id))
}

export function reportedConfig(options: any[] = []): { model?: string; reasoning_effort?: string | null } {
  const model = options.find((o) => o.id === "model")?.currentValue
  const effort = effortOption(options)?.currentValue
  return { ...(typeof model === "string" ? { model } : {}), ...(typeof effort === "string" ? { reasoning_effort: effort === "default" ? null : effort } : {}) }
}

export function requireEffort(options: any[], effort: string): string {
  const option = effortOption(options)
  if (!option) throw new Error("当前 Harness 或模型不支持指定思考强度，请将 Bot 的思考强度设为默认或更换模型")
  const values = (option.options ?? []).flatMap((o: any) => o.options ?? [o]).map((o: any) => o.value)
  if (!values.includes(effort)) throw new Error(`当前模型不支持思考强度 ${effort}，可用值：${values.join("、")}`)
  return option.id
}
