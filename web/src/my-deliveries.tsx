import { useEffect, useState } from "react"
import { ExternalLinkIcon, LinkIcon, UploadIcon } from "lucide-react"
import { api, withWs } from "@/api"
import { size } from "@/attach"
import { Badge } from "@/components/affect/badge-clean"
import { Card, CardContent, CardHeader } from "@/components/affect/card-clean"
import { Button } from "@/components/ui/button"
import { Empty } from "@/components/ui/empty"
import { Spinner } from "@/components/ui/spinner"
import { DeliveryShares } from "@/delivery-shares"
import { deliveryForm, type Delivery } from "@/delivery"
import { useT } from "@/i18n"
import { date } from "@/i18n/format"
import { Page } from "@/ui"

/** "我的发布": what this user published (steve publish, or a Bot outside an item), newest first. Only the owner
 *  sees it; each row opens its delivery page and manages its shares in place. */
export function MyDeliveriesView() {
  const t = useT(), [rows, setRows] = useState<Delivery[] | null>(null), [error, setError] = useState(""), [open, setOpen] = useState<string | null>(null)
  useEffect(() => {
    let live = true
    api.myDeliveries().then((r) => { if (live) setRows(Array.isArray(r) ? r : []) }, () => { if (live) setError(t("pages:delivery.mineError")) })
    return () => { live = false }
  }, [t])
  return <Page title={t("pages:delivery.mine")} sub={t("pages:delivery.mineHint")} max={760}>
    <div className="flex flex-col gap-3" data-my-deliveries>
      {error && <p role="alert" className="type-callout text-destructive">{error}</p>}
      {!rows && !error && <div role="status" className="flex justify-center py-8"><Spinner className="size-5" /></div>}
      {rows?.length === 0 && <Empty title={t("pages:delivery.mine")} text={t("pages:delivery.mineEmpty")} />}
      {rows?.map((d) => {
        const html = deliveryForm(d) === "html"
        return <Card key={d.id} data-my-delivery={d.id}>
          <CardHeader title={d.title || d.name} description={`${d.name} · ${size(d.size)} · ${date(d.created, { dateStyle: "medium", timeStyle: "short" })}`} icon={<UploadIcon />} />
          <CardContent className="flex min-w-0 flex-col gap-3">
            <div className="flex flex-wrap items-center gap-2">
              {html && <Badge tone={d.interactive ? "info" : "neutral"} dot>{t(d.interactive ? "pages:delivery.interactive" : "pages:delivery.static")}</Badge>}
              <Button size="sm" variant="outline" render={<a href={withWs(`/deliveries/${encodeURIComponent(d.id)}`)} target="_blank" rel="noreferrer" />} nativeButton={false} data-my-delivery-open><ExternalLinkIcon />{t("pages:delivery.open")}</Button>
              <Button size="sm" variant={open === d.id ? "secondary" : "ghost"} onClick={() => setOpen(open === d.id ? null : d.id)} aria-expanded={open === d.id} data-my-delivery-shares><LinkIcon />{t("pages:delivery.manageShares")}</Button>
            </div>
            {open === d.id && <DeliveryShares key={d.id} id={d.id} />}
          </CardContent>
        </Card>
      })}
    </div>
  </Page>
}
