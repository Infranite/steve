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
        // JSON is the original report, not the human summary or a sanitized reconstruction.
        let json = format!("{}\n", serde_json::to_string_pretty(&report).unwrap());
        assert_eq!(
            json,
            case["json"].as_str().unwrap(),
            "{} / JSON",
            case["id"]
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "sg14_output/cli.rs"]
mod cli;
