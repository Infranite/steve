import { useState } from "react"
import { DownloadIcon, ExternalLinkIcon, FileTextIcon } from "lucide-react"
import { withWs, type RAnnotation } from "@/api"
import { Card, CardContent, CardHeader } from "@/components/affect/card-clean"
import { Button } from "@/components/ui/button"
import { useT } from "@/i18n"
import { outputDeliveryId } from "@/delivery"
import { groupOuts } from "@/out-groups"
import { OutDialog, type OutOpen } from "@/item-outputs"
import type { Out } from "@/store"

/** Use the same artifacts as the item's outputs tab; local legacy outputs keep their workspace preview. */
export function DeliveryCards({ item, outs, anns }: { item: string; outs: Out[]; anns: RAnnotation[] }) {
  const t = useT(), [view, setView] = useState<OutOpen | null>(null)
  const rows = groupOuts(outs).final
  if (!rows.length) return null
  return <div className="flex min-w-0 flex-col gap-2" data-delivery-cards>
    {rows.map(({ o }) => {
      const id = outputDeliveryId(o)
      return <Card key={o[3]} data-delivery-card={id ?? "legacy"} data-artifact={o[3]} data-item={item}>
        <CardHeader title={o[1]} description={t("pages:delivery.fromItem", { item })} icon={<FileTextIcon />} />
        <CardContent className="flex min-w-0 flex-col gap-2">
          <div className="flex flex-wrap gap-2">
            {id && <Button size="sm" variant="outline" render={<a href={withWs(`/deliveries/${encodeURIComponent(id)}`)} target="_blank" rel="noreferrer" />} nativeButton={false} data-delivery-open><ExternalLinkIcon />{t("pages:delivery.open")}</Button>}
            <Button size="sm" variant="outline" onClick={() => setView({ o })} data-delivery-preview><FileTextIcon />{t("pages:outputs.preview")}</Button>
            {id && <Button size="sm" variant="ghost" render={<a href={withWs(`/api/file-deliveries/${encodeURIComponent(id)}/raw`)} download />} nativeButton={false} data-delivery-download><DownloadIcon />{t("pages:delivery.download")}</Button>}
          </div>
          {!id && <p className="type-footnote text-muted-foreground" data-legacy-delivery>{t("pages:delivery.legacy")}</p>}
        </CardContent>
      </Card>
    })}
    <OutDialog item={item} outs={outs} anns={anns} view={view} setView={setView} />
  </div>
}
