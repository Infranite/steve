// A deterministic post-enumeration child: the test orders the parent to create an adapter only
// after Hub::kill has scanned its children. Its independent group matches ACP's spawn contract.
// Closing the test-owned control connection also ends the process, including on assertion failure.
import { connect } from "node:net"
import { spawn } from "node:child_process"
import { createInterface } from "node:readline"
import { once } from "node:events"

const [role, gate] = process.argv.slice(2)
const control = connect(gate)
control.on("error", () => process.exit(1))
control.on("close", () => process.exit(0))
await once(control, "connect")
control.write(`${role} ${process.pid}\n`)
for await (const line of createInterface({ input: control })) {
  if (role === "parent" && line === "spawn") {
    spawn(process.execPath, [process.argv[1], "adapter", gate], {
      detached: true, stdio: "ignore", env: process.env,
    }).unref()
  } else if (role === "adapter" && line === "log") {
    // This is the existing fake's actual synchronous spawn record, held behind the socket gate.
    await import("../../../steve-acp/tests/fake-acp.js")
    control.write("logged\n")
  } else {
    throw new Error(`Unexpected gate command: ${role} ${line}`)
  }
}
