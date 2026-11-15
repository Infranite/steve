//! Human service summaries. The manager's report and exit semantics remain unchanged;
//! rendering never probes a service, reads logs, or infers runtime identity from a PID.
use serde_json::Value;
use std::fmt::Write;
use std::path::Path;
use steve_i18n::t;

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
    let _ = writeln!(
        out,
        "{label}{}{}",
        t!("install.status.label_separator"),
        visible(value)
    );
}

fn build(value: &Value) -> Option<String> {
    let version = value["version"].as_str().filter(|s| !s.is_empty())?;
    let commit = value["commit"].as_str().filter(|s| !s.is_empty());
    Some(match commit {
        Some(commit) => t!(
            "install.status.build",
            version = version,
            commit = commit.chars().take(12).collect::<String>()
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

fn diagnostic(text: &str) -> String {
    match text {
        "service is not installed" => t!("install.status.diag.not_installed"),
        "service is stopped or state is unknown" => t!("install.status.diag.stopped_or_unknown"),
        "Node is not paired; install never creates or approves a pairing" => {
            t!("install.status.diag.node_unpaired")
        }
        "Node identity is missing" => t!("install.status.diag.node_identity_missing"),
        "connection and active sessions require Hub Devices verification" => {
            t!("install.status.diag.verify_in_hub")
        }
        _ => text.to_owned(),
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
        "doctor" => t!("install.status.title.doctor"),
        "install" => t!("install.status.title.install"),
        "update" => t!("install.status.title.update"),
        "uninstall" => t!("install.status.title.uninstall"),
        "start" => t!("install.status.start"),
        "stop" => t!("install.status.title.stop"),
        "restart" => t!("install.status.title.restart"),
        _ => t!("install.status.title.status"),
    };
    let mut out = format!("Steve {name} · {title}\n");
    let state = report.get("service").unwrap_or(report);
    let label = report["label"].as_str().or(selected_label);
    let installed = state["installed"].as_bool();

    if action == "uninstall" {
        line(
            &mut out,
            &t!("install.status.label.service"),
            &if report["changed"] == true {
                t!("install.status.unregistered_now")
            } else {
                t!("install.status.nothing_to_unregister")
            },
        );
        if report["home_preserved"] == true {
            line(
                &mut out,
                &t!("install.status.label.data"),
                &t!("install.status.home_kept"),
            );
        }
        if let Some(archive) = report["archive"].as_str() {
            line(
                &mut out,
                &t!("install.status.label.service_backup"),
                archive,
            );
        }
    } else if matches!(action, "install" | "update") {
        line(
            &mut out,
            &t!("install.status.label.service_config"),
            &if report["changed"] == true {
                t!("install.status.config_updated")
            } else {
                t!("install.status.config_unchanged")
            },
        );
        if report["started"] == false {
            line(
                &mut out,
                &t!("install.status.start"),
                &t!("install.status.not_started"),
            );
        }
    } else {
        let running = match state["running"].as_bool() {
            Some(true) => match state["pid"].as_u64() {
                Some(pid) => t!("install.status.running_pid", pid = pid),
                None => t!("install.status.running"),
            },
            Some(false) => t!("install.status.stopped"),
            None => t!("install.status.running_unknown"),
        };
        line(&mut out, &t!("install.status.label.process"), &running);
        line(
            &mut out,
            &t!("install.status.label.registration"),
            &match installed {
                Some(true) => t!("install.status.registered"),
                Some(false) => t!("install.status.not_registered"),
                None => t!("install.status.unknown"),
            },
        );
        if matches!(action, "start" | "restart") {
            line(
                &mut out,
                &t!("install.status.label.operation"),
                &t!("install.status.request_accepted"),
            );
        }
    }

    if action != "uninstall" {
        let configured = report
            .get("configured_build")
            .or_else(|| report.get("build"));
        line(
            &mut out,
            &t!("install.status.label.configured_build"),
            &configured
                .and_then(build)
                .unwrap_or_else(|| t!("install.status.unknown")),
        );
        line(
            &mut out,
            &t!("install.status.label.running_build"),
            &build(&report["running_build"])
                .unwrap_or_else(|| t!("install.status.running_build_unverified")),
        );
        if let Some(error) = report["build_error"].as_str() {
            line(&mut out, &t!("install.status.label.package_check"), error);
        }
        if let Some(release) = report["configured_release"]
            .as_str()
            .or_else(|| report["release"].as_str())
        {
            line(&mut out, &t!("install.status.label.package"), release);
        }
        if role == "node" {
            line(
                &mut out,
                &t!("install.status.label.pairing"),
                &match report["pairing"]["configured"].as_bool() {
                    Some(true) => t!("install.status.pairing_configured"),
                    Some(false) => t!("install.status.pairing_missing"),
                    None => t!("install.status.unverified"),
                },
            );
            line(
                &mut out,
                &t!("install.status.label.hub_connection"),
                &t!("install.status.see_devices"),
            );
        }
        if let Some(candidate) = build(&report["candidate"]["build"]) {
            line(&mut out, &t!("install.status.label.candidate"), &candidate);
        }
    }
    line(
        &mut out,
        &t!("install.status.label.home"),
        &home.to_string_lossy(),
    );
    if let Some(label) = label {
        line(&mut out, &t!("install.status.label.service_name"), label);
    }

    if action == "doctor" {
        line(
            &mut out,
            &t!("install.status.label.check_scope"),
            &t!("install.status.check_scope"),
        );
        match report["diagnostics"].as_array() {
            Some(issues) if issues.is_empty() => line(
                &mut out,
                &t!("install.status.label.check_result"),
                &t!("install.status.no_issues"),
            ),
            Some(issues) => {
                let _ = writeln!(
                    out,
                    "{}",
                    t!("install.status.issues_heading", count = issues.len())
                );
                for issue in issues.iter().filter_map(Value::as_str) {
                    let _ = writeln!(out, "  - {}", visible(&diagnostic(issue)));
                }
            }
            None => line(
                &mut out,
                &t!("install.status.label.check_result"),
                &t!("install.status.no_diagnostics"),
            ),
        }
    }

    if action != "uninstall" {
        let next = if installed == Some(false) {
            line(
                &mut out,
                &t!("install.status.label.recovery"),
                &t!("install.status.recovery_hint"),
            );
            None
        } else if action == "doctor" && state["running"] == true {
            line(
                &mut out,
                &t!("install.status.label.next"),
                &if role == "hub" {
                    t!("install.status.next_hub_checks")
                } else {
                    t!("install.status.next_node_checks")
                },
            );
            None
        } else if state["running"].is_null() && !matches!(action, "install" | "update") {
            line(
                &mut out,
                &t!("install.status.label.next"),
                &t!("install.status.next_logs"),
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
            line(&mut out, &t!("install.status.label.next"), &next);
        }
        if let Some(update) = report["update_command"].as_str() {
            line(&mut out, &t!("install.status.label.update_command"), update);
        }
        match report["manager"].as_str() {
            Some("systemd-user") => {
                if let Some(label) = label
                    .and_then(|l| steve_install::common::shell_quote(&format!("{l}.service")).ok())
                {
                    line(
                        &mut out,
                        &t!("install.status.label.log_command"),
                        &format!("journalctl --user -u {label} --no-pager -n 50"),
                    );
                }
            }
            Some("launchd") => line(
                &mut out,
                &t!("install.status.label.log_file"),
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
