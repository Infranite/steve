use steve_flow::{nodes::wait::parse_until, policy::Staleness};
#[test]
fn runtime_dst_table() {
    for (name, text) in [
        ("America/New_York", "2026-03-08 02:15"),
        ("Europe/London", "2026-10-25 01:30"),
    ] {
        let z = steve_runtime::tz::Zone::parse(name).unwrap();
        let wall = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
        let ms = z.instant(wall).unwrap();
        assert_eq!(
            parse_until(
                &serde_json::json!(wall.format("%H:%M").to_string()),
                z.day(ms).unwrap().0,
                name.parse().unwrap()
            ),
            Some(ms)
        );
        let minute = (chrono::Timelike::hour(&wall) * 60 + chrono::Timelike::minute(&wall)) as u16;
        assert_eq!(
            Staleness::StaleAfter(minute).stale_at(ms, None, name.parse().unwrap()),
            Some(ms)
        );
    }
}
