//! A stand-in for the Rust Hub in ops/test-release.sh: /healthz, the web build from STEVE_WEB_DIR, and a schema
//! migration (schema_version → SCHEMA plus a table, like the real Hub's 004-011), `--build-info` with that schema.
//! mode.txt: ok | fail-start | bad-health; schema.txt: the schema it knows and migrates to.
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;

const MODE: &str = include_str!("../mode.txt");
const SCHEMA: &str = include_str!("../schema.txt");

fn field(json: &str, k: &str) -> String {
    let pat = format!("\"{k}\":\"");
    json.find(&pat).map(|i| json[i + pat.len()..].split('"').next().unwrap_or("").to_string()).unwrap_or_default()
}

fn main() {
    let mode = MODE.trim();
    let schema: u32 = SCHEMA.trim().parse().unwrap_or(11);
    if std::env::args().any(|a| a == "--build-info") {
        println!("{{\"version\":\"fake\",\"commit\":\"fake\",\"target\":\"fake\",\"schema\":{schema}}}");
        return;
    }
    eprintln!("fake steve-hub ({mode}) starting");
    if mode == "fail-start" {
        eprintln!("fake steve-hub: failing at start on purpose");
        std::process::exit(1);
    }
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let (version, web, home) = (env("STEVE_VERSION"), env("STEVE_WEB_DIR"), env("STEVE_HOME"));
    // the real Hub migrates at boot, before it listens
    let db = format!("{home}/data/steve.sqlite3");
    if Path::new(&db).exists() {
        let ok = std::process::Command::new("sqlite3")
            .args([&db, &format!("CREATE TABLE IF NOT EXISTS fake_rust_only (id TEXT PRIMARY KEY); UPDATE schema_version SET v = {schema} WHERE v < {schema};")])
            .status()
            .is_ok_and(|s| s.success());
        eprintln!("fake steve-hub: migrated {db}: {ok}");
    }
    let port: u16 = env("STEVE_PORT").parse().unwrap_or(8440);
    let l = TcpListener::bind(("127.0.0.1", port)).expect("listen");
    println!("fake steve-hub {version} ({mode}) on 127.0.0.1:{port}");
    for s in l.incoming() {
        let Ok(mut s) = s else { continue };
        let Ok(c) = s.try_clone() else { continue };
        let mut r = BufReader::new(c);
        let mut line = String::new();
        if r.read_line(&mut line).is_err() {
            continue;
        }
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) <= 2 {
                break;
            }
        }
        let path = line.split_whitespace().nth(1).unwrap_or("/").split('?').next().unwrap_or("/").to_string();
        let web_version = field(&std::fs::read_to_string(format!("{web}/release.json")).unwrap_or_default(), "version");
        let (code, ctype, cache, body): (&str, &str, &str, Vec<u8>) = if path == "/healthz" {
            let v = if mode == "bad-health" { "not-this-one" } else { version.as_str() };
            ("200 OK", "application/json", "", format!("{{\"ok\":true,\"version\":\"{v}\",\"web_version\":\"{web_version}\",\"hub\":\"fake-rust\"}}").into_bytes())
        } else if path.starts_with("/assets/") && !path.contains("..") {
            match std::fs::read(format!("{web}{path}")) {
                Ok(b) => ("200 OK", "text/javascript", "public, max-age=31536000, immutable", b),
                Err(_) => ("404 Not Found", "text/plain", "", b"not found".to_vec()),
            }
        } else {
            ("200 OK", "text/html", "no-store", std::fs::read(format!("{web}/index.html")).unwrap_or_default())
        };
        let cc = if cache.is_empty() { String::new() } else { format!("cache-control: {cache}\r\n") };
        let _ = write!(s, "HTTP/1.1 {code}\r\ncontent-type: {ctype}\r\n{cc}content-length: {}\r\nconnection: close\r\n\r\n", body.len());
        let _ = s.write_all(&body);
    }
}
