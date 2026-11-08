//! Human service summaries. The manager's report and exit semantics remain unchanged;
//! rendering never probes a service, reads logs, or infers runtime identity from a PID.
use serde_json::Value;
use std::fmt::Write;
use std::path::Path;

fn visible(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn line(out: &mut String, label: &str, value: &str) {
    let _ = writeln!(out, "{label}：{}", visible(value));
}

fn build(value: &Value) -> Option<String> {
    let version = value["version"].as_str().filter(|s| !s.is_empty())?;
    let commit = value["commit"].as_str().filter(|s| !s.is_empty());
    Some(match commit {
        Some(commit) => format!(
            "{version}（{}）",
            commit.chars().take(12).collect::<String>()
        ),
        None => version.to_owned(),
    })
}

fn command(role: &str, action: &str, home: &Path, label: Option<&str>) -> Option<String> {
    let home = steve_install::common::shell_quote(home.to_str()?).ok()?;
    let mut command = format!("steve {role} {action} --home {home}");
    if let Some(label) = label {
        let label = steve_install::common::shell_quote(label).ok()?;
        let _ = write!(command, " --label {label}");
    }
    Some(command)
}

fn diagnostic(text: &str) -> &str {
    match text {
        "service is not installed" => "服务尚未注册",
        "service is stopped or state is unknown" => "服务已停止，或服务管理器无法确认状态",
        "Node is not paired; install never creates or approves a pairing" => {
            "Node 尚未配对；安装服务不会自动配对或批准设备"
        }
        "Node identity is missing" => "Node 身份文件缺失",
        "connection and active sessions require Hub Devices verification" => {
            "连接和活动会话需要在 Hub 的设备页核验"
        }
        _ => text,
    }
}

pub fn service(
    role: &str,
    action: &str,
    home: &Path,
    selected_label: Option<&str>,
    report: &Value,
) -> String {
    let name = if role == "hub" { "Hub" } else { "Node" };
    let title = match action {
        "doctor" => "检查",
        "install" => "安装",
        "update" => "更新",
        "uninstall" => "注销",
        "start" => "启动",
        "stop" => "停止",
        "restart" => "重启",
        _ => "状态",
    };
    let mut out = format!("Steve {name} · {title}\n");
    let state = report.get("service").unwrap_or(report);
    let label = report["label"].as_str().or(selected_label);
    let installed = state["installed"].as_bool();

    if action == "uninstall" {
        line(
            &mut out,
            "服务",
            if report["changed"] == true {
                "已注销"
            } else {
                "未注册，无需注销"
            },
        );
        if report["home_preserved"] == true {
            line(&mut out, "数据", "安装目录和数据已保留");
        }
        if let Some(archive) = report["archive"].as_str() {
            line(&mut out, "服务定义备份", archive);
        }
    } else if matches!(action, "install" | "update") {
        line(
            &mut out,
            "服务配置",
            if report["changed"] == true {
                "已更新"
            } else {
                "已是目标配置"
            },
        );
        if report["started"] == false {
            line(&mut out, "启动", "本次操作未启动服务");
        }
    } else {
        let running = match state["running"].as_bool() {
            Some(true) => match state["pid"].as_u64() {
                Some(pid) => format!("运行中（PID {pid}）"),
                None => "运行中".into(),
            },
            Some(false) => "已停止".into(),
            None => "未知（服务管理器未确认）".into(),
        };
        line(&mut out, "服务进程", &running);
        line(
            &mut out,
            "服务注册",
            match installed {
                Some(true) => "已注册",
                Some(false) => "未注册",
                None => "未知",
            },
        );
        if matches!(action, "start" | "restart") {
            line(
                &mut out,
                "操作",
                "服务管理器已接受请求；应用就绪情况尚未核验",
            );
        }
    }

    if action != "uninstall" {
        let configured = report
            .get("configured_build")
            .or_else(|| report.get("build"));
        line(
            &mut out,
            "已配置版本",
            &configured.and_then(build).unwrap_or_else(|| "未知".into()),
        );
        line(
            &mut out,
            "运行版本",
            &build(&report["running_build"])
                .unwrap_or_else(|| "未核验（不能由已配置版本或 PID 推断）".into()),
        );
        if let Some(error) = report["build_error"].as_str() {
            line(&mut out, "安装包检查", error);
        }
        if let Some(release) = report["configured_release"]
            .as_str()
            .or_else(|| report["release"].as_str())
        {
            line(&mut out, "安装包", release);
        }
        if role == "node" {
            line(
                &mut out,
                "设备配对",
                match report["pairing"]["configured"].as_bool() {
                    Some(true) => "已配置配对（不代表已连接）",
                    Some(false) => "尚未配置",
                    None => "未核验",
                },
            );
            line(
                &mut out,
                "Hub 连接与活动会话",
                "未核验；请在 Hub 的设备页查看",
            );
        }
        if let Some(candidate) = build(&report["candidate"]["build"]) {
            line(&mut out, "候选版本", &candidate);
        }
    }
    line(&mut out, "数据目录", &home.to_string_lossy());
    if let Some(label) = label {
        line(&mut out, "服务名", label);
    }

    if action == "doctor" {
        line(
            &mut out,
            "检查范围",
            "本地静态检查；未探测 HTTP、OIDC 或模型可用性",
        );
        match report["diagnostics"].as_array() {
            Some(issues) if issues.is_empty() => {
                line(&mut out, "检查结果", "本次静态检查未发现问题")
            }
            Some(issues) => {
                let _ = writeln!(out, "检查结果（{} 项）：", issues.len());
                for issue in issues.iter().filter_map(Value::as_str) {
                    let _ = writeln!(out, "  - {}", visible(diagnostic(issue)));
                }
            }
            None => line(&mut out, "检查结果", "未提供诊断结果"),
        }
    }

    if action != "uninstall" {
        let next = if installed == Some(false) {
            line(
                &mut out,
                "恢复提示",
                "先核对安装目录；setup 仅用于首次或续接初始化，已有数据请保留并按部署说明恢复服务注册",
            );
            None
        } else if action == "doctor" && state["running"] == true {
            line(
                &mut out,
                "下一步",
                if role == "hub" {
                    "在配置的浏览器地址检查 /healthz 和登录；这些检查尚未执行"
                } else {
                    "到 Hub 的设备页核对同一 Node 的连接和活动会话"
                },
            );
            None
        } else if state["running"].is_null() && !matches!(action, "install" | "update") {
            line(
                &mut out,
                "下一步",
                "先查看日志确认服务管理器状态；不要据此重复安装、更新或注销服务",
            );
            None
        } else {
            let next = if matches!(action, "install" | "update") || state["running"] == false {
                "start"
            } else {
                "doctor"
            };
            command(role, next, home, label)
        };
        if let Some(next) = next {
            line(&mut out, "下一步", &next);
        }
        if let Some(update) = report["update_command"].as_str() {
            line(&mut out, "更新入口", update);
        }
        match report["manager"].as_str() {
            Some("systemd-user") => {
                if let Some(label) = label
                    .and_then(|l| steve_install::common::shell_quote(&format!("{l}.service")).ok())
                {
                    line(
                        &mut out,
                        "日志入口",
                        &format!("journalctl --user -u {label} --no-pager -n 50"),
                    );
                }
            }
            Some("launchd") => line(
                &mut out,
                "日志文件",
                &home
                    .join("logs")
                    .join(format!("{role}.log"))
                    .to_string_lossy(),
            ),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summaries_preserve_uncertainty_and_cover_service_receipt_shapes() {
        let cases = [
            (
                "hub",
                "doctor",
                json!({"service":{"installed":true,"running":true},"diagnostics":[]}),
                vec!["本次静态检查未发现问题", "检查 /healthz 和登录"],
                vec!["下一步：steve hub status", "登录已通过"],
            ),
            (
                "node",
                "status",
                json!({"service":{"installed":false,"running":false}}),
                vec!["服务注册：未注册", "已有数据请保留"],
                vec!["下一步：steve setup", "下一步：steve node start"],
            ),
            (
                "hub",
                "status",
                json!({"service":{"installed":true,"running":true,"pid":7},"configured_build":{"version":"1.2.3"},"running_build":null}),
                vec!["运行中（PID 7）", "已配置版本：1.2.3", "运行版本：未核验"],
                vec!["运行版本：1.2.3"],
            ),
            (
                "hub",
                "stop",
                json!({"installed":true,"running":false,"loaded":false}),
                vec!["服务进程：已停止", "steve hub start"],
                vec!["运行中"],
            ),
            (
                "node",
                "status",
                json!({"service":{"running":null},"pairing":{"configured":true}}),
                vec![
                    "服务进程：未知",
                    "已配置配对（不代表已连接）",
                    "Hub 连接与活动会话：未核验",
                ],
                vec!["连接成功"],
            ),
            (
                "hub",
                "install",
                json!({"installed":true,"changed":true,"started":false,"build":{"version":"1.2.3"}}),
                vec!["服务配置：已更新", "本次操作未启动服务", "steve hub start"],
                vec!["服务进程：运行中"],
            ),
            (
                "node",
                "update",
                json!({"installed":true,"changed":false,"started":false,"build":{"version":"1.2.4"}}),
                vec!["已是目标配置", "已配置版本：1.2.4"],
                vec!["运行版本：1.2.4"],
            ),
            (
                "hub",
                "uninstall",
                json!({"installed":false,"changed":true,"home_preserved":true,"archive":"backup"}),
                vec!["服务：已注销", "数据已保留", "服务定义备份：backup"],
                vec!["steve hub start", "运行版本"],
            ),
            (
                "node",
                "doctor",
                json!({"service":{"running":false},"diagnostics":["service is stopped or state is unknown","harness.json is not valid JSON"],"private":"fixture-secret"}),
                vec![
                    "检查结果（2 项）",
                    "harness.json is not valid JSON",
                    "未探测 HTTP、OIDC 或模型",
                ],
                vec!["fixture-secret", "未发现问题"],
            ),
        ];
        for (role, action, report, wanted, absent) in cases {
            let out = service(
                role,
                action,
                Path::new("/tmp/Steve Test"),
                Some("test.label"),
                &report,
            );
            for text in wanted {
                assert!(out.contains(text), "{action}: missing {text}: {out}");
            }
            for text in absent {
                assert!(!out.contains(text), "{action}: unexpected {text}: {out}");
            }
            assert!(out.contains("数据目录：/tmp/Steve Test"));
        }
    }
}
