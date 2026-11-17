/* ⌘K's 新待办: opens the todo page with its quick-add field focused. Apart from the page, which loads lazily. */
import { go } from "@/ui"

let pending = false
export const openQuickAdd = () => { pending = true; go("#/todos") }
/** whether the page should focus its field: once, for the request that opened it */
export const takeQuickAdd = () => { const p = pending; pending = false; return p }
