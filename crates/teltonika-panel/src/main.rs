//! `teltonika-panel`: serves the network panel for a Teltonika RUTM50 router and TSW202
//! switches, and sets up the devices' Event Juggler hooks.

use std::net::SocketAddr;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand, builder::BoolishValueParser};
use teltonika::Client;
use teltonika::juggler::{Rule, replace_rules};
use teltonika_panel::{Attached, Device, Devices, server};

#[derive(Parser)]
#[command(
    version,
    about = "Web panel for a Teltonika RUTM50 router and TSW202 switches"
)]
struct Cli {
    #[command(flatten)]
    devices: DeviceArgs,
    #[command(flatten)]
    serve: ServeArgs,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the panel (the default command).
    Serve,
    /// Create the Event Juggler rules that notify the panel about port and mobile data events.
    /// Replaces rules named panel_* and keeps all other rules. Skips devices that do not answer.
    SetupHooks {
        /// URL the devices post events to, as they reach this computer.
        #[arg(long, env = "PANEL_HOOK_URL")]
        hook_url: String,
    },
}

#[derive(Args)]
struct ServeArgs {
    /// Address to listen on.
    #[arg(long, env = "PANEL_LISTEN", default_value = "0.0.0.0", global = true)]
    listen: std::net::IpAddr,
    /// Port to listen on.
    #[arg(long, env = "PORT", default_value_t = 8080, global = true)]
    port: u16,
    /// Let any host change device settings. Only for the mock stack, where the browser
    /// reaches the panel through Docker's network and never from loopback.
    #[arg(long, env = "PANEL_ALLOW_REMOTE_CONTROL", value_parser = BoolishValueParser::new(), default_value = "false", global = true)]
    allow_remote_control: bool,
}

#[derive(Args)]
struct DeviceArgs {
    /// Router address; may include a port, e.g. localhost:8441.
    #[arg(
        long,
        env = "RUTM50_HOST",
        default_value = "192.168.30.1",
        global = true
    )]
    rutm50_host: String,
    #[arg(long, env = "RUTM50_USERNAME", default_value = "admin", global = true)]
    rutm50_username: String,
    #[arg(long, env = "RUTM50_PASSWORD", hide_env_values = true, global = true)]
    rutm50_password: Option<String>,
    #[arg(
        long,
        env = "TSW202_HOST",
        default_value = "192.168.30.2",
        global = true
    )]
    tsw202_host: String,
    #[arg(long, env = "TSW202_USERNAME", default_value = "admin", global = true)]
    tsw202_username: String,
    #[arg(long, env = "TSW202_PASSWORD", hide_env_values = true, global = true)]
    tsw202_password: Option<String>,
    /// Second switch. It uses the first switch's credentials unless overridden.
    #[arg(
        long,
        env = "TSW202_2_HOST",
        default_value = "192.168.30.3",
        global = true
    )]
    tsw202_2_host: String,
    #[arg(long, env = "TSW202_2_USERNAME", global = true)]
    tsw202_2_username: Option<String>,
    #[arg(long, env = "TSW202_2_PASSWORD", hide_env_values = true, global = true)]
    tsw202_2_password: Option<String>,
    /// Devices cabled to router ports that the diagram shows beside the uplinks, as
    /// "LAN4=Packet radio@192.168.30.50". Separate several with ";". The IP is optional.
    #[arg(long, env = "RUTM50_ATTACHED", value_delimiter = ';', global = true)]
    rutm50_attached: Vec<Attached>,
}

impl DeviceArgs {
    fn build(self) -> anyhow_lite::Result<Devices> {
        let need = |v: Option<String>, name: &str| {
            v.ok_or_else(|| {
                format!(
                    "set {name} (or pass --{})",
                    name.to_lowercase().replace('_', "-")
                )
            })
        };
        let rut_pw = need(self.rutm50_password, "RUTM50_PASSWORD")?;
        let sw_pw = need(self.tsw202_password, "TSW202_PASSWORD")?;
        let dev = |label: &str, host: &str, user: &str, pw: &str| -> anyhow_lite::Result<Device> {
            Ok(Device {
                label: label.into(),
                client: Arc::new(Client::new(host, user, pw).map_err(|e| e.to_string())?),
            })
        };
        Ok(Devices {
            router: dev("RUTM50", &self.rutm50_host, &self.rutm50_username, &rut_pw)?,
            switches: vec![
                dev(
                    "TSW202 #1",
                    &self.tsw202_host,
                    &self.tsw202_username,
                    &sw_pw,
                )?,
                dev(
                    "TSW202 #2",
                    &self.tsw202_2_host,
                    self.tsw202_2_username
                        .as_deref()
                        .unwrap_or(&self.tsw202_username),
                    self.tsw202_2_password.as_deref().unwrap_or(&sw_pw),
                )?,
            ],
            attached: self.rutm50_attached,
        })
    }
}

// Errors at startup are only printed, so a string is enough.
mod anyhow_lite {
    pub type Result<T> = std::result::Result<T, String>;
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // A .env file in the working directory supplies defaults; real environment variables win.
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        // Color codes only on a terminal, not in service or container logs.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();
    let cli = Cli::parse();
    let devices = match cli.devices.build() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => {
            let args = cli.serve;
            let addr = SocketAddr::new(args.listen, args.port);
            if let Err(e) = server::run(devices, addr, args.allow_remote_control).await {
                eprintln!("error: {e}");
                return std::process::ExitCode::FAILURE;
            }
            std::process::ExitCode::SUCCESS
        }
        Command::SetupHooks { hook_url } => setup_hooks(&devices, &hook_url).await,
    }
}

async fn setup_hooks(devices: &Devices, hook_url: &str) -> std::process::ExitCode {
    let ports = Rule {
        name: "panel_ports",
        log_event: "Switch Events",
    };
    let mobile = Rule {
        name: "panel_mobile",
        log_event: "Mobile Data",
    };
    let mut plan: Vec<(&Device, Vec<Rule>)> = vec![(&devices.router, vec![ports, mobile])];
    for s in &devices.switches {
        plan.push((
            s,
            vec![Rule {
                name: "panel_ports",
                log_event: "Switch Events",
            }],
        ));
    }
    let mut failed = false;
    for (d, rules) in plan {
        match replace_rules(&d.client, "panel_", &rules, hook_url).await {
            Ok(lines) => lines
                .iter()
                .for_each(|l| println!("{} ({}): {l}", d.label, d.client.host)),
            Err(e) => {
                failed = true;
                println!("{} ({}): skipped, {e}", d.label, d.client.host);
            }
        }
    }
    if failed {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}
