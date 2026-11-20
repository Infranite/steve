//! Permanent old-implementation snapshots of the read-only Feishu card decoder.
use serde_json::{Value, json};
use steve_channels::adapters::feishu::card::{
    self, MAX_BLOCKS, MAX_DEPTH, MAX_FIELD, MAX_INPUT, MAX_TEXT,
};

fn widgets() -> Vec<(&'static str, Value)> {
    vec![
        (
            "plain",
            json!({"tag":"plain_text","content":"plain","bold":true}),
        ),
        ("text", json!({"tag":"text","text":"text"})),
        (
            "md",
            json!({"tag":"lark_md","content":"hello <at id='ou_guest'>fallback</at>"}),
        ),
        (
            "markdown-inline",
            json!({"tag":"markdown","elements":[{"tag":"text","text":"inline"},{"tag":"at","user_id":"ou_guest"}]}),
        ),
        (
            "markdown-content",
            json!({"tag":"markdown_v1","content":"**words**"}),
        ),
        (
            "markdown-fallback",
            json!({"tag":"markdown","fallback":{"tag":"plain_text","content":"fallback"}}),
        ),
        (
            "div",
            json!({"tag":"div","text":{"tag":"plain_text","content":"main"},"fields":[{"text":{"tag":"lark_md","content":"field"}}],"extra":{"tag":"button","text":{"tag":"plain_text","content":"extra"}}}),
        ),
        (
            "notation",
            json!({"tag":"div","text":{"tag":"plain_text","content":"small","text_size":"notation"}}),
        ),
        (
            "note",
            json!({"tag":"note","elements":[{"tag":"text","text":"note"},{"tag":"emoji","type":"smile"}]}),
        ),
        ("hr", json!({"tag":"hr"})),
        ("br", json!({"tag":"br"})),
        (
            "heading",
            json!({"tag":"heading","level":99,"content":"Title"}),
        ),
        (
            "heading-text",
            json!({"tag":"heading","level":0,"text":"small title"}),
        ),
        (
            "columns-buttons",
            json!({"tag":"column_set","columns":[{"tag":"column","elements":[{"tag":"button","text":"one"}]},{"tag":"column","elements":[{"tag":"button","text":"two"}]}]}),
        ),
        (
            "one-column",
            json!({"tag":"column_set","columns":[{"elements":[{"tag":"plain_text","content":"one"}]}]}),
        ),
        (
            "two-columns",
            json!({"tag":"column_set","columns":[{"elements":[{"tag":"plain_text","content":"one"}]},{"elements":[{"tag":"plain_text","content":"two"}]}]}),
        ),
        (
            "repeat",
            json!({"tag":"repeat","elements":[{"tag":"plain_text","content":"repeated"}]}),
        ),
        (
            "actions-interleaved",
            json!({"tag":"actions","actions":[{"tag":"button","text":"before"},{"tag":"input","placeholder":"write"},{"tag":"button","text":"after"}]}),
        ),
        (
            "callback",
            json!({"tag":"button","text":"execute","value":{"operation":"never-execute"},"type":"primary"}),
        ),
        (
            "open-url",
            json!({"tag":"button","text":"safe","url":"https://example.invalid/card"}),
        ),
        (
            "unsafe-url",
            json!({"tag":"button","url":"javascript:alert(1)","disabled":true}),
        ),
        (
            "image",
            json!({"tag":"img","img_key":"not-a-resource","alt":{"tag":"plain_text","content":"alt"}}),
        ),
        (
            "image-grid",
            json!({"tag":"img_combination","img_list":[{},{}]}),
        ),
        (
            "chart",
            json!({"tag":"chart","chart_spec":{"title":{"text":"Trend"},"data":[{"values":[1,2]},{"values":[3]}]}}),
        ),
        (
            "chart-object",
            json!({"tag":"chart","chartSpec":{"data":{"values":[1]}}}),
        ),
        ("audio", json!({"tag":"audio","file_key":"not-fetched"})),
        ("video", json!({"tag":"video"})),
        (
            "panel",
            json!({"tag":"collapsible_panel","header":{"title":{"tag":"plain_text","content":"More"}},"expanded":true,"elements":[{"tag":"text","text":"inside"}]}),
        ),
        (
            "panel-default",
            json!({"tag":"collapsible_panel","elements":[]}),
        ),
        (
            "form",
            json!({"tag":"form","elements":[{"tag":"input","label":"Name"}]}),
        ),
        (
            "clickable",
            json!({"tag":"interactive_container","behaviors":[{"type":"open_url","default_url":"https://example.invalid/"}],"elements":[{"tag":"text","text":"inside"}]}),
        ),
        ("person", json!({"tag":"person_v1","user_id":"ou_guest"})),
        (
            "person-fallback",
            json!({"tag":"avatar","userID":"unknown","notation":{"tag":"plain_text","content":"display"}}),
        ),
        (
            "people",
            json!({"tag":"person_list","persons":[{"id":"ou_guest"},{"user_id":"unknown"}]}),
        ),
        (
            "inline",
            json!({"tag":"link","url":"https://example.invalid/","text":"link"}),
        ),
        (
            "list",
            json!({"tag":"list","items":[{"type":"ol","order":3,"level":0,"elements":[{"tag":"text","text":"ordered"}]},{"type":"ul","level":99,"elements":[{"tag":"text","text":"nested"}]}]}),
        ),
        (
            "quote-content",
            json!({"tag":"blockquote","content":"one\ntwo"}),
        ),
        (
            "quote-elements",
            json!({"tag":"blockquote","elements":[{"tag":"text","text":"quoted"}]}),
        ),
        (
            "code",
            json!({"tag":"code_block","language":"RUST","contents":[{"contents":[{"content":"let x = 1;"}]}]}),
        ),
        (
            "fallback",
            json!({"tag":"fallback_text","elements":[{"tag":"text","text":"kept"}]}),
        ),
        (
            "unknown-text",
            json!({"tag":"future_widget","content":"","text":"kept","elements":[{"tag":"text","text":"not preferred"}]}),
        ),
        (
            "unknown-children",
            json!({"tag":"future_widget","elements":[{"tag":"plain_text","content":"child"}]}),
        ),
        ("unknown-empty", json!({"tag":"future_widget"})),
        (
            "table",
            json!({"tag":"table","columns":[{"name":"a","display_name":"A","data_type":"text"}],"rows":[{"a":"one"},{"a":{"tag":"plain_text","content":"two"}}]}),
        ),
    ]
}
fn controls() -> Vec<(&'static str, Value)> {
    vec![
        (
            "overflow",
            json!({"tag":"overflow","options":["one",{"text":{"tag":"plain_text","content":"two"}},""]}),
        ),
        (
            "select-camel",
            json!({"tag":"select_static","selectedValues":["b"],"initialOption":"a","initialIndex":1,"options":[{"value":"a","text":"Alpha"},{"value":"b","text":{"tag":"plain_text","content":"Beta"}}]}),
        ),
        (
            "select-snake",
            json!({"tag":"select_static","selected_values":["b"],"initial_option":"a","initial_index":1,"options":[{"value":"a","text":"Alpha"},{"value":"b","text":"Beta"}]}),
        ),
        (
            "select-person",
            json!({"tag":"select_person","selected_values":["ou_guest","unlisted"]}),
        ),
        (
            "multi-person",
            json!({"tag":"multi_select_person","initial_option":"ou_guest"}),
        ),
        (
            "multi-static",
            json!({"tag":"multi_select_static","selected_values":["v"],"options":[{"value":"v"}]}),
        ),
        (
            "select-placeholder",
            json!({"tag":"select_static","placeholder":"Pick"}),
        ),
        ("select-empty", json!({"tag":"select_static"})),
        (
            "select-images",
            json!({"tag":"select_img","options":[{},{}]}),
        ),
        (
            "input-default",
            json!({"tag":"input","label":"Name","default_value":"Ada","placeholder":"ignored"}),
        ),
        ("input-camel", json!({"tag":"input","defaultValue":"value"})),
        (
            "input-placeholder",
            json!({"tag":"input","label":"Name","placeholder":"write"}),
        ),
        ("input-empty", json!({"tag":"input"})),
        (
            "date",
            json!({"tag":"date_picker","initial_date":"2026-11-21"}),
        ),
        (
            "date-alias",
            json!({"tag":"picker_date","initialDate":"2026-11-21"}),
        ),
        ("time", json!({"tag":"picker_time","initialTime":"12:34"})),
        (
            "datetime",
            json!({"tag":"picker_datetime","initial_datetime":"2026-11-21 12:34"}),
        ),
        (
            "time-placeholder",
            json!({"tag":"picker_time","placeholder":"later"}),
        ),
        ("date-empty", json!({"tag":"picker_date"})),
        (
            "checked",
            json!({"tag":"checker","checked":true,"text":"done"}),
        ),
        (
            "unchecked",
            json!({"tag":"checker","checked":false,"text":"todo"}),
        ),
    ]
}
fn cases() -> Vec<(String, String)> {
    let mut cases = Vec::new();
    for (name, node) in widgets().into_iter().chain(controls()) {
        for format in ["schema1", "schema2", "raw", "user_dsl", "flattened"] {
            let root = match format {
                "schema1" => {
                    json!({"header":{"title":{"tag":"plain_text","content":"Header"}},"elements":[node]})
                }
                "flattened" => json!({"title":"Header","elements":[[node]]}),
                _ => {
                    json!({"schema":"2.0","header":{"title":{"tag":"plain_text","content":"Header"}},"body":{"elements":[node]}})
                }
            };
            let outer = match format {
                "raw" => {
                    json!({"json_card":root.to_string(),"card_schema":2,"json_attachment":{"persons":{"ou_guest":{"mention_key":"@_user_1"}}}})
                }
                "user_dsl" => json!({"user_dsl":root.to_string()}),
                _ => root,
            };
            cases.push((format!("{format}/{name}"), outer.to_string()));
        }
    }
    cases.push(("raw-property-aliases".into(),json!({"json_card":json!({"schema":2,"body":{"property":{"elements":[{"tag":"div","property":{"text":{"tag":"plain_text","property":{"content":"raw property","textStyle":{"size":"notation"}}}}},{"tag":"select_static","property":{"initialIndex":0,"options":[{"value":"v","text":{"tag":"plain_text","property":{"content":"raw option"}}}]}}]}}}).to_string(),"json_attachment":"{}"}).to_string()));
    cases.push(("i18n-order".into(),json!({"i18n_header":{"en_us":{"title":{"tag":"plain_text","content":"English"}},"zh_cn":{"title":{"tag":"plain_text","content":"中文"}}},"i18n_elements":{"en_us":[{"tag":"plain_text","content":"en"}],"zh_cn":[{"tag":"plain_text","content":"中"}]}}).to_string()));
    cases
}
#[test]
fn sg21_card_all_widget_and_control_shapes_match_the_original_goldens() {
    let mentions = vec![
        ("@_user_1".into(), "Ada".into()),
        ("ou_guest".into(), "Ada".into()),
    ];
    let actual: Vec<Value> = cases()
        .into_iter()
        .map(|(name, content)| {
            let c = card::parse(&content, &mentions);
            json!({"name":name,"text":c.text,"attachment":c.attachment})
        })
        .collect();
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sg21/card-goldens.json");
    if std::env::var_os("SG21_GENERATE_GOLDENS").is_some() {
        use std::io::Write;
        // Generation is first-write-only and runs on the old implementation before commit 1.
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("goldens must not already exist");
        f.write_all(&serde_json::to_vec_pretty(&actual).unwrap())
            .unwrap();
        return;
    }
    let expected: Vec<Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(actual.len(), expected.len());
    for (a, e) in actual.iter().zip(expected.iter()) {
        assert_eq!(a, e, "{}", a["name"]);
    }
}
#[test]
fn sg21_card_limits_and_callback_payloads_stay_read_only() {
    let parse = |nodes: Vec<Value>| {
        card::parse(
            &json!({"schema":"2.0","body":{"elements":nodes}}).to_string(),
            &[],
        )
    };
    for n in [MAX_BLOCKS, MAX_BLOCKS + 1] {
        let c = parse(vec![json!({"tag":"hr"}); n]);
        assert_eq!(c.attachment["blocks"].as_array().unwrap().len(), MAX_BLOCKS);
        assert_eq!(c.attachment["truncated"], n > MAX_BLOCKS);
    }
    let mut node = json!({"tag":"plain_text","content":"at the bottom"});
    for _ in 0..MAX_DEPTH + 2 {
        node = json!({"tag":"form","elements":[node]});
    }
    let c = parse(vec![node]);
    assert_eq!(c.attachment["truncated"], true);
    let c = parse(vec![
        json!({"tag":"input","default_value":"字".repeat(MAX_FIELD+1)}),
    ]);
    assert_eq!(c.attachment["truncated"], true);
    let c = parse(vec![
        json!({"tag":"plain_text","content":"x".repeat(MAX_FIELD)});
        5
    ]);
    assert_eq!(c.text.chars().count(), MAX_TEXT + 1);
    assert_eq!(c.attachment["truncated"], true);
    let exact = format!("{}{{}}", " ".repeat(MAX_INPUT - 2));
    assert_ne!(card::parse(&exact, &[]).text, "[卡片：内容过大]");
    assert_eq!(card::parse(&(exact + " "), &[]).text, "[卡片：内容过大]");
    let c = parse(vec![
        json!({"tag":"button","text":"Click","value":{"operation":"never-execute"},"url":"javascript:alert(1)"}),
    ]);
    let b = &c.attachment["blocks"][0]["items"][0];
    assert_eq!(b["callback"], true);
    assert!(b.get("value").is_none());
    assert!(b.get("url").is_none());
    assert!(!c.text.contains("never-execute"));
}
