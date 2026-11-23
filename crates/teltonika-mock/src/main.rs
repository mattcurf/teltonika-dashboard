//! Runs one mock device: `teltonika-mock rutm50` or `teltonika-mock tsw202`.
//! Settings come from MOCK_* environment variables; see the README.

use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use teltonika_mock::{Auth, Juggler, Mock, env_or, rutm50, serve, tsw202};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let auth = Auth {
        username: env_or("MOCK_USERNAME", "admin".into()),
        password: env_or("MOCK_PASSWORD", "admin01".into()),
        token_ttl: Duration::from_secs(
            env_or("MOCK_TOKEN_TTL", "299".into())
                .parse()
                .unwrap_or(299),
        ),
    };
    let port: u16 = env_or("MOCK_PORT", "443".into()).parse().unwrap_or(443);
    let listener = match TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port))) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: port {port}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let hook = std::env::var("MOCK_HOOK_URL")
        .ok()
        .filter(|u| !u.is_empty());
    // Pre-creates the Event Juggler rules that `teltonika-panel setup-hooks` makes on real devices.
    let juggler = |rules: &[(&str, &str)]| {
        let mut j = Juggler::default();
        if let Some(url) = &hook {
            j.seed(url, rules);
        }
        j
    };
    let result = match std::env::args().nth(1).as_deref() {
        Some("rutm50") => {
            let j = juggler(&[
                ("panel_ports", "Switch Events"),
                ("panel_mobile", "Mobile Data"),
            ]);
            serve(
                Mock::new(
                    "RUTM50",
                    rutm50::Rutm50::new(rutm50::Config::from_env()),
                    auth,
                    j,
                ),
                listener,
            )
            .await
        }
        Some("tsw202") => {
            let j = juggler(&[("panel_ports", "Switch Events")]);
            serve(
                Mock::new(
                    "TSW202",
                    tsw202::Tsw202::new(tsw202::Config::from_env()),
                    auth,
                    j,
                ),
                listener,
            )
            .await
        }
        _ => {
            eprintln!("usage: teltonika-mock rutm50|tsw202");
            return std::process::ExitCode::from(2);
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
