//! Runs the fake IMAP, SMTP, Gmail and Graph servers plus the control
//! interface, for trying the web app against fake mailboxes (W17).
//!
//! ```text
//! steve-fake-mail [--user me@example.com] [--password app-password]
//!                 [--starttls] [--seed] [--ca-out <path>]
//! ```
//!
//! Prints one JSON line with the ports, the control URL and the path of the
//! CA certificate (to pass as `STEVE_MAIL_TEST_CA`), then serves until killed.

use std::path::PathBuf;

use steve_mail::fake::control::{ControlServer, Fakes};
use steve_mail::fake::{
    FakeGmail, FakeGraph, FakeImap, FakeSmtp, ImapConfig, SmtpConfig, TestCa, TlsMode, fixtures,
};

struct Args {
    user: String,
    password: String,
    starttls: bool,
    seed: bool,
    ca_out: Option<PathBuf>,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        user: "me@example.com".into(),
        password: "app-password".into(),
        starttls: false,
        seed: false,
        ca_out: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--user" => a.user = it.next().ok_or("--user needs a value")?,
            "--password" => a.password = it.next().ok_or("--password needs a value")?,
            "--starttls" => a.starttls = true,
            "--seed" => a.seed = true,
            "--ca-out" => a.ca_out = Some(it.next().ok_or("--ca-out needs a value")?.into()),
            "-h" | "--help" => {
                return Err("usage: steve-fake-mail [--user U] [--password P] [--starttls] [--seed] [--ca-out PATH]".into());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(a)
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(a).await {
        eprintln!("steve-fake-mail: {e}");
        std::process::exit(1);
    }
}

async fn run(a: Args) -> Result<(), Box<dyn std::error::Error>> {
    let ca = TestCa::generate()?;
    let tls = if a.starttls {
        TlsMode::StartTls
    } else {
        TlsMode::Implicit
    };
    let accounts = vec![(a.user.clone(), a.password.clone())];
    let imap = FakeImap::start(ImapConfig {
        tls,
        ca: Some(ca.clone()),
        accounts: accounts.clone(),
        condstore: true,
        ..Default::default()
    })
    .await?;
    let smtp = FakeSmtp::start(SmtpConfig {
        tls,
        ca: Some(ca.clone()),
        accounts,
        deliver_to: Some(imap.clone()),
        ..Default::default()
    })
    .await?;
    let gmail = FakeGmail::start(&a.user).await?;
    let graph = FakeGraph::start(&a.user).await?;
    if a.seed {
        for (_, raw) in fixtures::ALL {
            imap.inject(&a.user, "INBOX", raw)?;
            gmail.inject(raw, &["INBOX", "UNREAD"]);
            graph.inject("inbox", raw)?;
        }
    }
    let ca_path = a.ca_out.unwrap_or_else(|| {
        std::env::temp_dir().join(format!("steve-fake-mail-{}-ca.pem", std::process::id()))
    });
    std::fs::write(&ca_path, ca.ca_pem())?;
    let control = ControlServer::start(Fakes {
        imap: Some(imap.clone()),
        smtp: Some(smtp.clone()),
        gmail: Some(gmail.clone()),
        graph: Some(graph.clone()),
        default_user: a.user.clone(),
    })
    .await?;
    println!(
        "{}",
        serde_json::json!({
            "user": a.user,
            "imap": {"host": imap.host(), "port": imap.port(), "tls": format!("{tls:?}")},
            "smtp": {"host": smtp.host(), "port": smtp.port(), "tls": format!("{tls:?}")},
            "gmail_api_base": gmail.api_base(),
            "graph_api_base": graph.api_base(),
            "control": control.url(),
            "ca_pem_path": ca_path,
        })
    );
    std::future::pending::<()>().await;
    Ok(())
}
