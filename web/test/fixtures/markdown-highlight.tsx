import { useState } from "react"
import { createRoot } from "react-dom/client"
import { Markdown } from "../../src/components/markdown"
import "../../src/index.css"

const code = "const result = 42"
function Fixture() {
  const [closed, close] = useState(false)
  return <>
    <button onClick={() => close(true)}>Close fence</button>
    <Markdown text={"```js\n" + code + (closed ? "\n```" : "")} streaming />
  </>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
