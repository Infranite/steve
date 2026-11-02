import { StrictMode, useEffect, useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { MessageResource } from "../../src/channels/message-resource"
import CardMarkdown from "../../src/channels/card-markdown"

function Fixture() {
  const [show, setShow] = useState(true)
  useEffect(() => { const clear = () => setShow(false); window.addEventListener("fixture-clear", clear); return () => window.removeEventListener("fixture-clear", clear) }, [])
  return <main style={{ maxWidth: 720, margin: "auto", padding: 20 }}>
    <h1>消息资源</h1>
    {show && <div className="flex flex-col gap-3" data-fixture-resources>
      <MessageResource url="/api/test/image" name="示例图片" image />
      <MessageResource url="/api/test/file" name="项目计划.pdf" />
      <MessageResource url="/api/test/fail" name="失败图片" image />
      <MessageResource url="/api/test/html" name="伪装图片" image />
      <MessageResource name="不可用图片" image />
      <MessageResource url="/api/test/huge" name="超大附件.zip" />
    </div>}
    <CardMarkdown text={'**加粗文本**\n\n[安全链接](https://example.com/path) [危险链接](javascript:alert(1))\n\n![外部图片](https://tracking.example.test/image.png)\n\n<script>window.cardScriptExecuted=true</script>\n\n```html\n<img src="https://tracking.example.test/html.png">\n```'} />
  </main>
}
createRoot(document.getElementById("root")!).render(<StrictMode><Fixture /></StrictMode>)
