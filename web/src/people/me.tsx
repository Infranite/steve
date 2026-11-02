/* #/contacts/me — the user's own contact: the contact page with is_me (no delete, no relation menu, 时区 / 语言 / 称呼 first) */
import { ContactView } from "@/people/contact"

export function MeView() { return <ContactView id="me" /> }
