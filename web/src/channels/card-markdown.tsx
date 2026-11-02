/* Provider-authored card text: no HTML preview, embedded remote images, or executable widgets. */
import { Streamdown, defaultRehypePlugins, defaultRemarkPlugins, type Components } from "streamdown"
import remarkBreaks from "remark-breaks"
import { cardLink } from "@/channels/card-link"

const components: Components = {
  a: ({ href, children }) => {
    const safe = cardLink(href)
    return safe ? <a href={safe} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer" className="text-blue underline break-all">{children}</a> : <span>{children}</span>
  },
  img: ({ alt }) => <span className="text-muted-foreground">[图片{alt ? `：${alt}` : ""}]</span>,
  code: ({ children }) => <code className="whitespace-pre-wrap break-all rounded bg-muted px-1">{children}</code>,
}

export default function CardMarkdown({ text }: { text: string }) {
  return <Streamdown className="md min-w-0 break-words [overflow-wrap:anywhere]" components={components}
    remarkPlugins={[...Object.values(defaultRemarkPlugins), remarkBreaks]}
    rehypePlugins={[defaultRehypePlugins.sanitize, defaultRehypePlugins.harden]}
    controls={false} mode="static">{text}</Streamdown>
}
