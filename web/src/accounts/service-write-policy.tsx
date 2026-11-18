import type { ServiceWritePolicy } from "@/api/accounts"
import { useT } from "@/i18n"

/** The channel service's constraint, not the permission granted to Steve. */
export function ServiceWritePolicyNote({ policy }: { policy: ServiceWritePolicy }) {
  const t = useT()
  return <span data-service-write-policy={policy}>{t(`accounts:conn.serviceWrite.${policy}`)}</span>
}
