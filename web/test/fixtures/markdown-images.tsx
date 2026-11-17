import { StrictMode, useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Markdown } from "../../src/components/markdown"
import { setPageWorkspace } from "../../src/api"
import { currentHub } from "../../src/hub"
import { packageUrl } from "../../src/delivery"
import { setLang } from "../../src/i18n"

const query = new URLSearchParams(location.search), hub = currentHub()
// Deliberately different from the image's source workspace and the Hub's workspace cookie.
setPageWorkspace("image-other")
const image = `/api/files/${encodeURIComponent(query.get("file") ?? "missing")}?ws=default&fixture=markdown%20image#preview`
const entry = `${hub}/api/file-deliveries/${encodeURIComponent(query.get("delivery") ?? "missing")}/content/docs/README.md?ws=default`
const resolve = (ref: string) => packageUrl(ref, entry, "docs/README.md")
function Fixture() {
  const [changed, setChanged] = useState(false)
  return <main className="mx-auto flex max-w-[720px] flex-col gap-4 p-5">
    <h1>Markdown 图片契约（合成测试数据）</h1>
    <section data-case="relative"><Markdown text={`![当前 Hub 图片](${image})`} /></section>
    <section data-case="reference"><Markdown text={`![引用式图片][picture]\n\n[picture]: ${image}`} /></section>
    <section data-case="absolute"><Markdown text={`![完整 Hub 图片](${hub}${image})`} /></section>
    <section data-case="missing"><Markdown text="![缺失图片](/api/files/missing-image?ws=default)" /></section>
    <section data-case="change"><Markdown text={`![变更地址图片](${changed ? image : "/api/files/changed-image?ws=default"})`} /><button type="button" onClick={() => setChanged(true)}>Change image source</button></section>
    <section data-case="package"><Markdown text={"![交付包相对图片](../assets/test.png)\n\n[包内说明](../about.md)\n\n![越界图片](../../../../private/test.png)"} resolve={resolve} /></section>
    <section data-case="local"><Markdown text={"![本地路径图片](/private/tmp/steve-image-test.png)\n\n![相对本地图片](generated/test.png)"} /></section>
    <section data-case="mixed"><Markdown text={`内联图片 ![图文混排](${image}) 后面的文字。`} /></section>
    <section data-case="external"><Markdown text="![外部图片](https://images.example.test/test.png)" /></section>
    <section data-case="unsafe"><Markdown text={'![脚本图片](javascript:alert(1))\n\n![文件协议](file:///tmp/test.png)\n\n![伪装脚本图片](data:image/svg+xml;base64,PHN2Zy8+)\n\n[危险链接](javascript:alert(1))\n\n<script>window.markdownImageExecuted=true</script>\n\n<img src="/private/tmp/html-test.png" onerror="window.markdownImageExecuted=true">\n\n```md\n![代码示例](/private/tmp/code-test.png)\n```'} /></section>
    <section data-case="streaming"><Markdown text="![正在生成的图片](" streaming /></section>
    <button type="button" onClick={() => setLang("en")}>Switch to English</button>
  </main>
}
createRoot(document.getElementById("root")!).render(<StrictMode><Fixture /></StrictMode>)
