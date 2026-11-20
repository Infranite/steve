//! Characterization of the existing renderer, with manager reports supplied as data.
steve_i18n::init!();
#[path = "../src/output.rs"]
mod output;

use serde_json::Value;
use std::path::Path;

fn fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/sg14/service.json")).unwrap()
}

#[test]
fn service_report_text_matrix_is_byte_exact_and_never_mutates_reports() {
    let cases = fixtures();
    for case in &cases {
        let report = case["report"].clone();
        for locale in ["zh-CN", "en"] {
            let rendered = steve_i18n::with(locale, || {
                output::service(
                    case["role"].as_str().unwrap(),
                    case["action"].as_str().unwrap(),
                    Path::new(case["home"].as_str().unwrap()),
                    case["label"].as_str(),
                    &report,
                )
            });
            assert_eq!(
                rendered,
                case[locale].as_str().unwrap(),
                "{} / {locale}",
                case["id"]
            );
            assert_eq!(case["report"], report, "{}: report changed", case["id"]);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "sg14_output/cli.rs"]
mod cli;

#[test]
fn service_summary_release_label_and_recovery_precedence_branches() {
    use serde_json::json;
    let home = Path::new("/summary/Valid Home");
    assert!(steve_install::common::shell_quote(home.to_str().unwrap()).is_ok());
    for locale in ["zh-CN", "en"] {
        for case in [
            "configured_release",
            "invalid_label",
            "systemd_without_label",
            "recovery_before_unknown",
        ] {
            let mut report =
                json!({"manager":"systemd-user","service":{"installed":true,"running":false}});
            let selected = if case == "invalid_label" {
                Some("bad\nlabel")
            } else {
                None
            };
            if case == "configured_release" {
                report["configured_release"] = json!("/configured/package");
                report["release"] = json!("/legacy/package");
            }
            if case == "recovery_before_unknown" {
                report["service"] = json!({"installed":false,"running":null});
            }
            let rendered = steve_i18n::with(locale, || {
                output::service("hub", "status", home, selected, &report)
            });
            let (next, recovery, logs, package) = if locale == "en" {
                (
                    "Next: ",
                    "Recovery: ",
                    "Logs: ",
                    "Release package: /configured/package",
                )
            } else {
                (
                    "下一步：",
                    "恢复提示：",
                    "日志入口：",
                    "安装包：/configured/package",
                )
            };
            match case {
                "configured_release" => {
                    assert_eq!(
                        rendered
                            .lines()
                            .filter(|l| l.contains("/configured/package")
                                || l.contains("/legacy/package"))
                            .collect::<Vec<_>>(),
                        vec![package],
                        "{locale}"
                    );
                }
                "invalid_label" => {
                    assert!(rendered.contains("bad\\nlabel"));
                    assert!(
                        !rendered
                            .lines()
                            .any(|l| l.starts_with(next) || l.starts_with(logs)),
                        "{locale}: {rendered}"
                    );
                    let valid = steve_i18n::with(locale, || {
                        output::service("hub", "status", home, Some("valid.label"), &report)
                    });
                    assert!(
                        valid.contains("steve hub start"),
                        "valid home and label must produce the command"
                    );
                }
                "systemd_without_label" => {
                    let wanted = format!("{next}steve hub start --home '/summary/Valid Home'");
                    assert_eq!(
                        rendered
                            .lines()
                            .filter(|l| l.starts_with(next))
                            .collect::<Vec<_>>(),
                        vec![wanted.as_str()]
                    );
                    assert!(
                        !rendered.contains("journalctl")
                            && !rendered.contains("--label")
                            && !rendered.lines().any(|l| l.starts_with(logs))
                    );
                }
                _ => {
                    let wanted = if locale == "en" {
                        "Recovery: check the installation directory first; setup is only for a first or continued setup, so keep existing data and restore the service registration as the deployment guide describes"
                    } else {
                        "恢复提示：先核对安装目录；setup 仅用于首次或续接初始化，已有数据请保留并按部署说明恢复服务注册"
                    };
                    assert_eq!(
                        rendered
                            .lines()
                            .filter(|l| l.starts_with(recovery))
                            .collect::<Vec<_>>(),
                        vec![wanted]
                    );
                    assert!(
                        !rendered.lines().any(|l| l.starts_with(next)),
                        "recovery must precede unknown-state log advice: {rendered}"
                    );
                }
            }
        }
    }
}
