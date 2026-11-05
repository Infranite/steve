import { lazy, Suspense, type ComponentType, type ComponentProps } from "react"
import { Skeleton } from "@/components/ui/skeleton"
import { t } from "@/i18n"

export function lazyView<T extends ComponentType<any>>(load: () => Promise<{ default: T }>) {
  const View = lazy(async () => {
    try { return await load() } catch (e) {
      // A stale release may reference removed chunks; never loop when offline.
      const key = "steve-chunk-reload:" + (document.querySelector('script[type="module"][src]')?.getAttribute("src") ?? "dev")
      if (/chunk|dynamically imported|module script|Importing a module/i.test(String(e))) {
        try { if (!sessionStorage.getItem(key)) { sessionStorage.setItem(key, "1"); location.reload(); return await new Promise<never>(() => {}) } } catch {}
      }
      throw e
    }
  })
  return (props: ComponentProps<T>) => <Suspense fallback={<div className="space-y-4 p-6" role="status" aria-label={t("shell:misc.loading")}><Skeleton className="h-7 w-40" /><Skeleton className="h-48 w-full" /></div>}><View {...props} /></Suspense>
}
