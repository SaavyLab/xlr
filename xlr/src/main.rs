//! `xlr`: control a pro audio setup from the command line.
//!
//! Output is human-readable by default and stable JSON with `--json`, so
//! the same commands serve people and AI agents.

mod dante;
mod focusrite;
mod network;
mod route;
mod setup;

use clap::{Parser, Subcommand};
use std::{net::Ipv4Addr, process::ExitCode, time::Duration};

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// IPv4 address of the local interface on the audio network. Defaults to
    /// the interface the OS would use for multicast.
    #[arg(long, global = true, env = "XLR_INTERFACE")]
    interface: Option<Ipv4Addr>,

    /// How long to listen for devices, in milliseconds.
    #[arg(long, global = true, default_value_t = 1500)]
    discovery_ms: u64,

    /// Per-request timeout for device queries, in milliseconds.
    #[arg(long, global = true, default_value_t = 500)]
    timeout_ms: u64,

    /// Emit JSON instead of text.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show the whole setup: Dante devices and routes, and Focusrite
    /// interfaces connected over USB.
    Status,
    /// Focusrite interfaces connected to this machine over USB.
    #[command(subcommand)]
    Focusrite(FocusriteCommand),
    /// Route a receiver channel from a transmitter channel, or clear it.
    ///
    /// Channels are written `channel@device`; a receiver may also be given by
    /// number (`2@device`). The source must exist, the write is skipped when
    /// the route is already in place, and the result is read back.
    Route {
        /// The receiver to change, e.g. `Left@stage-box`.
        receiver: String,
        /// The transmitter to route from, e.g. `Mic 3@foh-rack`.
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        source: Option<String>,
        /// Remove the receiver's subscription instead.
        #[arg(long)]
        clear: bool,
        /// Show what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum FocusriteCommand {
    /// List connected Focusrite devices without opening them.
    Identify,
    /// Read input and monitor switch settings (read-only). Focusrite
    /// Control must not be running.
    Status,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("xlr: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    if let Command::Focusrite(command) = &cli.command {
        return run_focusrite(cli, command);
    }
    let interface = match cli.interface {
        Some(interface) => interface,
        None => network::default_interface()?,
    };
    let options = dante::Options {
        interface,
        discovery: Duration::from_millis(cli.discovery_ms),
        timeout: Duration::from_millis(cli.timeout_ms),
    };
    match cli.command {
        Command::Status => {
            let status = setup::read(&options);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print!("{}", status.render());
            }
            if status.has_errors() {
                eprintln!("xlr: some devices could not be read; see the `error` fields");
                return Ok(ExitCode::FAILURE);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Focusrite(_) => unreachable!("handled above"),
        Command::Route {
            ref receiver,
            ref source,
            clear: _,
            dry_run,
        } => {
            let outcome = route::run(&options, receiver, source.as_deref(), dry_run)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            } else {
                println!("{}", outcome.render());
            }
            Ok(if outcome.ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
    }
}

fn run_focusrite(
    cli: &Cli,
    command: &FocusriteCommand,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    match command {
        FocusriteCommand::Identify => {
            let identities = focusrite::identify()?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&identities)?);
            } else {
                print!("{}", focusrite::render_identities(&identities));
            }
            Ok(ExitCode::SUCCESS)
        }
        FocusriteCommand::Status => {
            let status = focusrite::status(Duration::from_millis(cli.timeout_ms));
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print!("{}", status.render());
            }
            Ok(if status.has_errors() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}
