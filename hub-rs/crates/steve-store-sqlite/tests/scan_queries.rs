#[path = "scan_support.rs"]
mod support;
use steve_storage_api::{entity::*, query::*, *};
use steve_types::Code;
use support::*;

#[test]
fn sparse_windows_limits_ties_and_continuation_do_not_skip_or_repeat() {
    let f = Fixture::new();
    f.seed(12);
    let filter = vec![Pred::Eq {
        field: "label".into(),
        value: Cell::Text("missing".into()),
    }];
    let first: Page<Row> = f.run(f.scan(4, 3, None, filter.clone())).unwrap();
    assert!(first.rows.is_empty());
    assert!(first.exhausted_budget);
    assert_eq!(first.resume.unwrap().key, vec![Cell::Int(1), Cell::Int(3)]);
    for (limit, budget) in [(1, 8), (8, 1), (2, 2), (3, 5), (12, 12)] {
        let mut resume = None;
        let mut ids = Vec::new();
        loop {
            let page: Page<Row> = f.run(f.scan(limit, budget, resume, vec![])).unwrap();
            ids.extend(page.rows.iter().map(|(r, _)| id(r)));
            resume = page.resume;
            if resume.is_none() {
                assert!(!page.exhausted_budget);
                break;
            }
        }
        assert_eq!(ids, (1..=12).collect::<Vec<_>>());
    }
    let exact:CountResult=f.run(f.q("count",serde_json::json!({"index":{"entity":"rec","name":"score"},"range":KeyRange::default(),"filter":[],"budget":12}))).unwrap();
    assert_eq!(exact, CountResult::Exact(12));
    let lower:CountResult=f.run(f.q("count",serde_json::json!({"index":{"entity":"rec","name":"score"},"range":KeyRange::default(),"filter":filter,"budget":3}))).unwrap();
    assert_eq!(lower, CountResult::AtLeast(0));
    let found: Option<(Row, Rev)> = f.run(f.get(2)).unwrap();
    assert_eq!(found.unwrap().1, Rev(2));
    let exists: bool = f
        .run(f.q(
            "exists",
            serde_json::json!({"key":{"entity":"rec","values":[Cell::Int(99)]}}),
        ))
        .unwrap();
    assert!(!exists);
}
#[test]
fn null_prefix_ranges_descending_and_binary_text_match_sql_order() {
    let f = Fixture::new();
    f.seed(6);
    f.writer.execute_batch("UPDATE rec SET score=NULL WHERE id=1; UPDATE rec SET label='a' WHERE id=1; UPDATE rec SET label='Z' WHERE id=2;").unwrap();
    let mut after = None;
    let mut ids = Vec::new();
    loop {
        let page: Page<Row> = f.run(f.scan(1, 2, after, vec![])).unwrap();
        ids.extend(page.rows.iter().map(|(r, _)| id(r)));
        after = page.resume;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(ids, (1..=6).collect::<Vec<_>>());
    let range = KeyRange {
        start: KeyBound::Included(vec![Cell::Int(1)]),
        end: KeyBound::Included(vec![Cell::Int(1)]),
    };
    let q=f.q("scan",serde_json::json!({"index":{"entity":"rec","name":"score"},"range":range,"filter":[],"order":"desc","limit":10,"budget":10}));
    let page: Page<Row> = f.run(q).unwrap();
    assert_eq!(
        page.rows.iter().map(|(r, _)| id(r)).collect::<Vec<_>>(),
        vec![5, 4, 3]
    );
    let q=f.q("scan",serde_json::json!({"index":{"entity":"rec","name":"label"},"range":KeyRange::default(),"filter":[],"order":"asc","limit":10,"budget":10}));
    let page: Page<Row> = f.run(q).unwrap();
    assert_eq!(id(&page.rows[0].0), 2);
    assert_eq!(id(&page.rows[1].0), 1);
    let bad=f.q("scan",serde_json::json!({"index":{"entity":"rec","name":"not_declared"},"range":KeyRange::default(),"filter":[],"limit":5,"budget":2}));
    assert_eq!(f.run::<Page<Row>>(bad).unwrap_err().code, Code::Invalid);
    let bad = f.scan(0, 2, None, vec![]);
    assert_eq!(f.run::<Page<Row>>(bad).unwrap_err().code, Code::Invalid);
    let bad = f.scan(2, 0, None, vec![]);
    assert_eq!(f.run::<Page<Row>>(bad).unwrap_err().code, Code::Invalid);
    let bad = f.scan(
        2,
        3,
        Some(ResumeToken {
            key: vec![Cell::Int(1)],
        }),
        vec![],
    );
    assert_eq!(f.run::<Page<Row>>(bad).unwrap_err().code, Code::Invalid);
}
#[test]
fn every_scalar_roundtrips_and_corrupt_enums_and_nonfinite_reals_are_rejected() {
    let f = Fixture::new();
    let cases = vec![
        (FieldType::Int, Cell::Int(i64::MIN)),
        (FieldType::Int, Cell::Int(i64::MAX)),
        (FieldType::Millis, Cell::Millis(i64::MAX)),
        (FieldType::Real, Cell::Real(f64::MAX)),
        (FieldType::Bytes, Cell::Bytes(vec![0, 1, 255])),
        (FieldType::Bool, Cell::Bool(true)),
        (FieldType::Text, Cell::Text("中文😀".repeat(1024 * 128))),
        (FieldType::Text, Cell::Text("x".repeat(1024 * 1024))),
        (
            FieldType::Json,
            Cell::Json(RawJson::from_str("{ \"z\": 2, \"a\": \"😀\" }").unwrap()),
        ),
        (
            FieldType::Enum(vec!["ready".into()]),
            Cell::Text("ready".into()),
        ),
    ];
    for (n, (ty, cell)) in cases.into_iter().enumerate() {
        let table = format!("scalar{n}");
        f.writer
            .execute_batch(&format!(
                "CREATE TABLE {table}(id INTEGER PRIMARY KEY,value,rev INTEGER NOT NULL)"
            ))
            .unwrap();
        let mut e = f.desc.clone();
        e.name = table.clone();
        e.fields = vec![
            e.fields[0].clone(),
            FieldDesc {
                name: "value".into(),
                ty,
                nullable: false,
            },
        ];
        e.indexes.clear();
        let value = steve_store_sqlite::scan::bind_cell(&e.fields[1], &cell).unwrap();
        f.writer
            .execute(&format!("INSERT INTO {table} VALUES(1,?1,9)"), [value])
            .unwrap();
        let q = request(
            &e,
            "get",
            serde_json::json!({"key":{"entity":table,"values":[Cell::Int(1)]}}),
        );
        let output =
            steve_store_sqlite::scan::query(&f.engine.read_conn().unwrap(), &e, q).unwrap();
        let row: Option<(Row, Rev)> = output.body.decode().unwrap();
        assert_eq!(row.unwrap().0.0[1].1, cell);
        if matches!(e.fields[1].ty, FieldType::Enum(_)) {
            f.writer
                .execute(&format!("UPDATE {table} SET value='unknown'"), [])
                .unwrap();
            let q = request(
                &e,
                "get",
                serde_json::json!({"key":{"entity":table,"values":[Cell::Int(1)]}}),
            );
            assert_eq!(
                steve_store_sqlite::scan::query(&f.engine.read_conn().unwrap(), &e, q)
                    .unwrap_err()
                    .code,
                Code::Incompatible
            );
        }
    }
    let field = FieldDesc {
        name: "real".into(),
        ty: FieldType::Real,
        nullable: false,
    };
    for n in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            steve_store_sqlite::scan::bind_cell(&field, &Cell::Real(n))
                .unwrap_err()
                .code,
            Code::Invalid
        );
    }
}

#[test]
fn compound_primary_keys_and_filtered_partial_ranges_keep_total_order() {
    let f = Fixture::new();
    f.writer.execute_batch("CREATE TABLE compound(id TEXT NOT NULL,part INTEGER NOT NULL,score INTEGER NOT NULL,rev INTEGER NOT NULL,PRIMARY KEY(id,part)); INSERT INTO compound VALUES('中',10,1,1),('中',2,1,1),('a',1,1,1),('z',1,2,1);").unwrap();
    let mut e = f.desc.clone();
    e.name = "compound".into();
    e.key = vec!["id".into(), "part".into()];
    e.fields = vec![
        FieldDesc {
            name: "id".into(),
            ty: FieldType::Text,
            nullable: false,
        },
        FieldDesc {
            name: "part".into(),
            ty: FieldType::Int,
            nullable: false,
        },
        FieldDesc {
            name: "score".into(),
            ty: FieldType::Int,
            nullable: false,
        },
    ];
    e.indexes = vec![IndexDesc {
        name: "score".into(),
        fields: vec!["score".into()],
        unique: false,
    }];
    let conn = f.engine.read_conn().unwrap();
    let mut resume = None;
    let mut parts = Vec::new();
    loop {
        let q = request(
            &e,
            "scan",
            serde_json::json!({"index":{"entity":"compound","name":"score"},"range":KeyRange{start:KeyBound::Included(vec![Cell::Int(1)]),end:KeyBound::Included(vec![Cell::Int(1)])},"filter":[Pred::Ne{field:"id".into(),value:Cell::Text("a".into())}],"limit":1,"budget":2,"resume":resume}),
        );
        let page: Page<Row> = steve_store_sqlite::scan::query(&conn, &e, q)
            .unwrap()
            .body
            .decode()
            .unwrap();
        for (row, _) in page.rows {
            parts.push(row.0[1].1.clone());
        }
        resume = page.resume;
        if resume.is_none() {
            break;
        }
        assert_eq!(resume.as_ref().unwrap().key.len(), 3);
    }
    assert_eq!(parts, vec![Cell::Int(2), Cell::Int(10)]);
}

#[test]
fn reaching_limit_does_not_traverse_the_remaining_budget_to_compute_metadata() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let f = Fixture::new();
    f.writer.execute_batch("WITH RECURSIVE n(id) AS (VALUES(1) UNION ALL SELECT id+1 FROM n WHERE id<10000) INSERT INTO rec SELECT id,id,'row',id FROM n").unwrap();
    let conn = f.engine.read_conn().unwrap();
    let steps = |budget, resume, expected| {
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        conn.progress_handler(
            1,
            Some(move || {
                observed.fetch_add(1, Ordering::Relaxed);
                false
            }),
        )
        .unwrap();
        let out =
            steve_store_sqlite::scan::query(&conn, &f.desc, f.scan(1, budget, resume, vec![]))
                .unwrap();
        let page: Page<Row> = out.body.decode().unwrap();
        assert_eq!(id(&page.rows[0].0), expected);
        conn.progress_handler(0, None::<fn() -> bool>).unwrap();
        count.load(Ordering::Relaxed)
    };
    let small = steps(2, None, 1);
    let large = steps(10000, None, 1);
    let resumed = steps(
        10000,
        Some(ResumeToken {
            key: vec![Cell::Int(9000), Cell::Int(9000)],
        }),
        9001,
    );
    assert!(
        resumed < small * 8 + 500,
        "continuation must seek rather than scan its prefix: {resumed}"
    );
    assert!(
        large < small * 8 + 500,
        "limit=1 must stop early; VM steps small={small}, large={large}"
    );
}
