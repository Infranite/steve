//! The bundled SQLite has what `kb_fts` needs: FTS5, contentless
//! tables with `contentless_delete`, `bm25` with column weights, and phrase matching over the
//! bigram token stream produced by `seg`.

use steve_knowledge::seg::{fts_query, seg};
use steve_store::testkit::knowledge::FtsFixture;

fn db(rows: &[(i64, &str, &str)]) -> FtsFixture {
    let fts = FtsFixture::new().unwrap();
    for (id, body, title) in rows {
        fts.put(*id, &seg(body), &seg(title)).unwrap();
    }
    fts
}

fn search(fts: &FtsFixture, q: &str) -> Vec<i64> {
    fts_query(q)
        .map(|terms| fts.search(&terms).unwrap())
        .unwrap_or_default()
}

#[test]
fn bigram_substrings_match() {
    let conn = db(&[
        (1, "部署脚本在 scripts 目录", "部署手册"),
        (2, "合同每年 3 月续签", "Acme 合同说明"),
        (3, "脚本部署", "反序"),
    ]);
    // A two-character word inside a longer run.
    assert_eq!(search(&conn, "部署"), vec![1, 3]);
    assert_eq!(search(&conn, "续签"), vec![2]);
    // A phrase keeps character order: “脚本部署” is not “部署脚本”.
    assert_eq!(search(&conn, "部署脚本"), vec![1]);
    // Middle bigram of a run, and a single character as prefix.
    assert_eq!(search(&conn, "署脚"), vec![1]);
    assert_eq!(search(&conn, "签"), Vec::<i64>::new());
    assert_eq!(search(&conn, "续"), vec![2]);
    // ASCII prefix and case folding.
    assert_eq!(search(&conn, "SCRIPT"), vec![1]);
    assert_eq!(search(&conn, "acme"), vec![2]);
    // Syntax in the query is inert: NOT is just a word, so 合同 is OR-ed in, not excluded.
    let mut ids = search(&conn, "部署 NOT 合同 \" ( *");
    ids.sort();
    assert_eq!(ids, vec![1, 2, 3]);
}

#[test]
fn title_weighs_more_than_body() {
    let conn = db(&[(1, "顺带提到续签", "周会"), (2, "其他内容", "续签安排")]);
    assert_eq!(search(&conn, "续签"), vec![2, 1]);
}

#[test]
fn contentless_delete_and_replace() {
    let conn = db(&[(1, "部署脚本", "a"), (2, "部署回滚", "b")]);
    conn.remove(1).unwrap();
    assert_eq!(search(&conn, "部署"), vec![2]);
    // Re-index the same rowid with new text (what the index queue does on change).
    conn.put(1, &seg("全新的内容"), "").unwrap();
    assert_eq!(search(&conn, "部署"), vec![2]);
    assert_eq!(search(&conn, "全新"), vec![1]);
    // Contentless: the text itself is not stored.
    let s = conn.content(1).unwrap();
    assert_eq!(s, None);
}

#[test]
fn full_width_emoji_and_diacritics() {
    let conn = db(&[(1, "ＡＰＩ网关限流🚀", ""), (2, "Café résumé", "")]);
    assert_eq!(search(&conn, "api"), vec![1]);
    assert_eq!(search(&conn, "限流"), vec![1]);
    assert_eq!(search(&conn, "🚀"), Vec::<i64>::new());
    assert_eq!(search(&conn, "cafe"), vec![2]);
    assert_eq!(search(&conn, "RESUME"), vec![2]);
}
