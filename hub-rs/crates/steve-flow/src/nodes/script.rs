//! `script`: `{code}` runs `export const handler = async (input) => …` in QuickJS (`rquickjs`) and
//! outputs what it returns.
//!
//! Sandbox: a fresh runtime per run with only the language itself plus `JSON`, `Math`, `Date`,
//! `RegExp`, `Map`/`Set` and promises; no module loader, no `console`, no network, no files (QuickJS
//! has none unless its std modules are added, and they are not). Limits: 1 s of wall time (the
//! interrupt handler stops the interpreter, also inside a `while (true) {}`), 16 MB of heap, a small
//! stack, and 256 KiB of JSON output. The engine runs on a blocking thread so a busy script never
//! holds up the runtime.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use rquickjs::context::intrinsic;
use rquickjs::{CatchResultExt, CaughtError, Context, Ctx, Function, Module, Runtime, Value as Js};
use serde_json::Value;

use super::{NodeKind, NodeOutcome, RunCx, req_str};

/// CPU budget of one run.
pub const TIME_LIMIT: Duration = Duration::from_secs(1);
/// Heap of one run.
pub const MEMORY_LIMIT: usize = 16 * 1024 * 1024;
/// Interpreter stack (the blocking thread has 2 MiB).
const STACK_LIMIT: usize = 512 * 1024;
/// Largest output, as JSON.
pub const OUTPUT_LIMIT: usize = 256 * 1024;
/// Longest code.
pub const CODE_LIMIT: usize = 64 * 1024;

pub const TIMED_OUT: &str = "脚本运行超过 1 秒，已停止";
pub const OUT_OF_MEMORY: &str = "脚本用的内存超过 16 MB，已停止";
pub const NO_HANDLER: &str = "脚本要导出 handler：export const handler = async (input) => …";
pub const NEVER_RESOLVED: &str = "脚本的 handler 没有返回结果（Promise 一直没有完成）";
pub const OUTPUT_TOO_BIG: &str = "脚本的输出太大了（最多 256 KiB）";

pub struct Script;

#[async_trait]
impl NodeKind for Script {
    fn kind(&self) -> &'static str {
        "script"
    }

    fn validate(&self, cfg: &Value) -> Result<(), String> {
        let code = req_str(cfg, "code", "脚本", CODE_LIMIT)?;
        if !code.contains("handler") {
            return Err(NO_HANDLER.into());
        }
        Ok(())
    }

    async fn run(&self, _cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        let code = cfg["code"].as_str().unwrap_or("").to_string();
        let input = input.clone();
        match tokio::task::spawn_blocking(move || run_script(&code, &input)).await {
            Ok(Ok(v)) => NodeOutcome::next(v),
            Ok(Err(e)) => NodeOutcome::Fail(e),
            Err(e) => NodeOutcome::Fail(format!("脚本出错：{e}")),
        }
    }
}

/// Runs `code`'s `handler(input)` under the limits. The error is the Chinese step error.
pub fn run_script(code: &str, input: &Value) -> Result<Value, String> {
    let rt = Runtime::new().map_err(|e| format!("脚本环境启动失败：{e}"))?;
    rt.set_memory_limit(MEMORY_LIMIT);
    rt.set_max_stack_size(STACK_LIMIT);
    let start = Instant::now();
    rt.set_interrupt_handler(Some(Box::new(move || start.elapsed() > TIME_LIMIT)));
    let ctx = Context::custom::<(
        intrinsic::Date,
        intrinsic::Eval,
        intrinsic::RegExpCompiler,
        intrinsic::RegExp,
        intrinsic::Json,
        intrinsic::MapSet,
        intrinsic::Promise,
    )>(&rt)
    .map_err(|e| format!("脚本环境启动失败：{e}"))?;
    let input_json = serde_json::to_string(input).unwrap_or_else(|_| "null".into());
    let out = ctx.with(|ctx| -> Result<String, String> {
        let fail = |e: CaughtError<'_>| describe(e, start);
        let (module, done) = Module::declare(ctx.clone(), "flow-script", code)
            .catch(&ctx)
            .map_err(fail)?
            .eval()
            .catch(&ctx)
            .map_err(fail)?;
        settle(&ctx, done.into_value(), start)?;
        let handler: Js = module
            .namespace()
            .and_then(|ns| ns.get("handler"))
            .catch(&ctx)
            .map_err(fail)?;
        let Some(handler) = handler.as_function().cloned() else {
            return Err(NO_HANDLER.into());
        };
        let handler: Function = handler;
        let arg = ctx.json_parse(input_json).catch(&ctx).map_err(fail)?;
        let ret: Js = handler.call((arg,)).catch(&ctx).map_err(fail)?;
        let ret = settle(&ctx, ret, start)?;
        let s = ctx.json_stringify(ret).catch(&ctx).map_err(fail)?;
        Ok(match s {
            Some(s) => s.to_string().catch(&ctx).map_err(fail)?,
            None => "null".into(),
        })
    })?;
    if out.len() > OUTPUT_LIMIT {
        return Err(OUTPUT_TOO_BIG.into());
    }
    serde_json::from_str(&out).map_err(|e| format!("脚本出错：{e}"))
}

/// Waits for a promise (driving the job queue, still under the interrupt handler); other values
/// as they are.
fn settle<'js>(ctx: &Ctx<'js>, v: Js<'js>, start: Instant) -> Result<Js<'js>, String> {
    let Some(p) = v.as_promise().cloned() else {
        return Ok(v);
    };
    match p.finish::<Js>().catch(ctx) {
        Ok(v) => Ok(v),
        // an interrupted async function leaves its promise pending
        Err(_) if start.elapsed() > TIME_LIMIT => Err(TIMED_OUT.into()),
        Err(CaughtError::Error(rquickjs::Error::WouldBlock)) => Err(NEVER_RESOLVED.into()),
        Err(e) => Err(describe(e, start)),
    }
}

fn describe(e: CaughtError<'_>, start: Instant) -> String {
    if start.elapsed() > TIME_LIMIT {
        return TIMED_OUT.into();
    }
    let msg = match &e {
        CaughtError::Exception(x) => {
            let m = x.message().unwrap_or_default();
            let name = x
                .get::<_, Option<String>>("name")
                .ok()
                .flatten()
                .unwrap_or_else(|| "Error".into());
            format!("{name}: {m}")
        }
        CaughtError::Value(v) => v
            .as_string()
            .and_then(|s| s.to_string().ok())
            .unwrap_or_else(|| format!("{:?}", v.type_of())),
        CaughtError::Error(e) => e.to_string(),
    };
    if msg.contains("out of memory") || matches!(e, CaughtError::Value(ref v) if v.is_null()) {
        return OUT_OF_MEMORY.into();
    }
    format!("脚本出错：{msg}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn async_handler_returns_its_value() {
        let code = "export const handler = async (input) => input.attachments.find(a => a.type === 'invoice')";
        let input = json!({"attachments": [{"type": "x"}, {"type": "invoice", "amount": 86.5}]});
        assert_eq!(
            run_script(code, &input).unwrap(),
            json!({"type": "invoice", "amount": 86.5})
        );
    }

    #[test]
    fn sync_handler_math_json_date() {
        let code = "export function handler(i) { return { n: Math.max(i.a, 3), s: JSON.stringify([1]), y: new Date(0).getUTCFullYear(), m: [...new Set([1,1,2])].length } }";
        assert_eq!(
            run_script(code, &json!({"a": 7})).unwrap(),
            json!({"n": 7, "s": "[1]", "y": 1970, "m": 2})
        );
    }

    #[test]
    fn infinite_loop_is_stopped_within_a_second() {
        let t = Instant::now();
        let e = run_script(
            "export const handler = async () => { while (true) {} }",
            &json!(null),
        )
        .unwrap_err();
        assert_eq!(e, TIMED_OUT);
        assert!(
            t.elapsed() < Duration::from_millis(1500),
            "{:?}",
            t.elapsed()
        );
        // also at module top level and in a promise job
        assert_eq!(
            run_script("for(;;){}", &json!(null)).unwrap_err(),
            TIMED_OUT
        );
        assert_eq!(
            run_script(
                "export const handler = () => Promise.resolve().then(() => { for(;;){} })",
                &json!(null)
            )
            .unwrap_err(),
            TIMED_OUT
        );
    }

    #[test]
    fn no_network_no_files_no_host() {
        let e = run_script(
            "export const handler = async () => fetch('http://127.0.0.1/')",
            &json!(null),
        )
        .unwrap_err();
        assert!(e.contains("ReferenceError"), "{e}");
        assert!(e.contains("fetch"), "{e}");
        for g in [
            "XMLHttpRequest",
            "require",
            "process",
            "std",
            "os",
            "console",
            "Deno",
            "WebSocket",
        ] {
            let code = format!("export const handler = () => typeof {g}");
            assert_eq!(
                run_script(&code, &json!(null)).unwrap(),
                json!("undefined"),
                "{g}"
            );
        }
        let e = run_script(
            "import * as os from 'os'; export const handler = () => os",
            &json!(null),
        )
        .unwrap_err();
        assert!(e.starts_with("脚本出错"), "{e}");
        let e = run_script(
            "export const handler = async () => (await import('std')).loadFile('/etc/passwd')",
            &json!(null),
        )
        .unwrap_err();
        assert!(e.starts_with("脚本出错"), "{e}");
    }

    #[test]
    fn memory_is_capped() {
        let e = run_script(
            "export const handler = () => { const a = []; for (;;) a.push('x'.repeat(1024) + a.length) }",
            &json!(null),
        )
        .unwrap_err();
        assert!(e == OUT_OF_MEMORY || e == TIMED_OUT, "{e}");
        let e = run_script(
            "export const handler = () => 'x'.repeat(32 * 1024 * 1024).length",
            &json!(null),
        )
        .unwrap_err();
        assert_eq!(e, OUT_OF_MEMORY);
    }

    #[test]
    fn errors_are_reported() {
        assert_eq!(
            run_script("export const h = 1", &json!(null)).unwrap_err(),
            NO_HANDLER
        );
        let e = run_script(
            "export const handler = () => { throw new TypeError('坏了') }",
            &json!(null),
        )
        .unwrap_err();
        assert_eq!(e, "脚本出错：TypeError: 坏了");
        let e = run_script(
            "export const handler = async () => { throw 'nope' }",
            &json!(null),
        )
        .unwrap_err();
        assert_eq!(e, "脚本出错：nope");
        assert!(
            run_script("export const handler = (", &json!(null))
                .unwrap_err()
                .starts_with("脚本出错：SyntaxError")
        );
        assert_eq!(
            run_script(
                "export const handler = () => new Promise(() => {})",
                &json!(null)
            )
            .unwrap_err(),
            NEVER_RESOLVED
        );
        assert_eq!(
            run_script(
                "export const handler = () => 'x'.repeat(300000)",
                &json!(null)
            )
            .unwrap_err(),
            OUTPUT_TOO_BIG
        );
        assert_eq!(
            run_script("export const handler = () => undefined", &json!(null)).unwrap(),
            json!(null)
        );
    }

    #[test]
    fn validate() {
        assert!(Script.validate(&json!({})).is_err());
        assert_eq!(
            Script.validate(&json!({"code": "1+1"})).unwrap_err(),
            NO_HANDLER
        );
        assert!(
            Script
                .validate(&json!({"code": "export const handler = (i) => i"}))
                .is_ok()
        );
    }
}
