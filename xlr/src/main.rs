//! `xlr`: control a pro audio setup from the command line.
//!
//! Output is human-readable by default and stable JSON with `--json`, so
//! the same commands serve people and AI agents.

mod address;
mod config;
mod dante;
mod focusrite;
mod identity;
mod network;
mod pairing;
mod pipewire;
mod remote;
mod route;
mod setup;
mod studio;
mod tls;
mod trust;

use clap::{Parser, Subcommand};
use std::{
    net::{Ipv4Addr, SocketAddr},
    process::ExitCode,
    time::Duration,
};

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
    /// Show the whole setup: Dante devices and routes, Focusrite
    /// interfaces on USB, and this host's PipeWire graph, labelled with
    /// your names.
    Status {
        /// Read only this machine, not the hosts in `xlr hosts`.
        #[arg(long)]
        local: bool,
    },
    /// Serve this host's hardware to paired machines.
    ///
    /// Other machines add this one with `xlr hosts add`, then you approve
    /// them here with `xlr peers approve`.
    Serve {
        /// Address and port to listen on.
        #[arg(long, default_value = "0.0.0.0:7373")]
        listen: SocketAddr,
    },
    /// Pair with another machine in both directions, with one approval.
    ///
    /// Pins the host, pre-approves it to read this machine, and asks it to
    /// pair. Approving on the host (`xlr peers approve <code>`) also adds this
    /// machine there as a host. Both machines should run `xlr serve`.
    Pair {
        /// Its address, e.g. `192.168.1.20`.
        address: String,
        /// A local alias; by default the host's own name (its `[host] name`).
        #[arg(long = "as")]
        alias: Option<String>,
        /// Expected fingerprint (from `xlr id` on that host).
        #[arg(long)]
        fingerprint: Option<String>,
        /// The port this machine's `xlr serve` listens on.
        #[arg(long, default_value_t = remote::DEFAULT_PORT)]
        port: u16,
        /// What the host may do on this machine.
        #[arg(long, value_enum, default_value_t = RoleArg::Read)]
        grant: RoleArg,
    },
    /// Print this machine's name and identity fingerprint.
    Id,
    /// Hosts this machine reads from. Lists them, with pairing state, when
    /// given no subcommand.
    Hosts {
        #[command(subcommand)]
        command: Option<HostsCommand>,
    },
    /// Machines allowed to read this host. Lists approved and pending peers
    /// when given no subcommand.
    Peers {
        #[command(subcommand)]
        command: Option<PeersCommand>,
    },
    /// List your names and check each against live hardware.
    ///
    /// Names are defined in `$XLR_CONFIG` or `~/.config/xlr/xlr.toml`:
    ///
    ///   [devices]
    ///   scarlett = "focusrite/<serial>"
    ///
    ///   [names]
    ///   guitar = "focusrite/scarlett/input/1"
    ///   desktop-left = "dante/<device>/rx/Left"
    ///   mac-out-9 = "dante/<device>/tx/<channel>"
    Names,
    /// Focusrite interfaces connected to this machine over USB.
    #[command(subcommand)]
    Focusrite(FocusriteCommand),
    /// Route a receiver channel from a transmitter channel, or clear it.
    ///
    /// Each side may be one of your names, an address
    /// (`dante/<device>/rx/<channel>`), or `channel@device`. A receiver may
    /// also be given by number (`2@device`). The source must exist, the write
    /// is skipped when the route is already in place, and the result is read
    /// back.
    Route {
        /// The receiver to change, e.g. `desktop-left` or `Left@stage-box`.
        receiver: String,
        /// The transmitter to route from, e.g. `mac-out-9` or `Mic 3@foh-rack`.
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
enum HostsCommand {
    /// Pin a host's identity and ask it to pair.
    Add {
        /// Its address, e.g. `192.168.1.20` or `mac-mini.local:7373`.
        address: String,
        /// A local alias; by default the host's own name (its `[host] name`).
        #[arg(long = "as")]
        alias: Option<String>,
        /// Expected fingerprint (from `xlr id` on that host). Without it, the
        /// first fingerprint seen is pinned and the pairing code confirms it.
        #[arg(long)]
        fingerprint: Option<String>,
    },
    /// Forget a host.
    Remove { name: String },
}

#[derive(Subcommand)]
enum PeersCommand {
    /// Approve a pending pairing request by its code or fingerprint.
    Approve {
        /// The six-digit code, or a fingerprint prefix of 8+ digits.
        selector: String,
        /// What the peer may do.
        #[arg(long, value_enum, default_value_t = RoleArg::Read)]
        role: RoleArg,
    },
    /// Revoke an approved peer by name or fingerprint prefix.
    Remove { selector: String },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum RoleArg {
    /// Status and other reads.
    Read,
    /// Reads plus changes.
    Control,
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
    if let Some(message) = run_pairing(&cli.command)? {
        println!("{message}");
        return Ok(ExitCode::SUCCESS);
    }
    let config = config::Config::load()?;
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
        Command::Status { local } => {
            let home = if local { None } else { config::home() };
            let status = studio::read(|| setup::read(&options, &config), home.as_deref());
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
        Command::Names => {
            let checks = setup::check_names(&config, &setup::read(&options, &config));
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&checks)?);
            } else if checks.is_empty() {
                println!(
                    "No names defined. Add a [names] table to {}; see `xlr names --help`.",
                    config.path.as_deref().map_or_else(
                        || "~/.config/xlr/xlr.toml".to_owned(),
                        |path| path.display().to_string()
                    )
                );
            } else {
                let width = checks
                    .iter()
                    .map(|check| check.name.len())
                    .max()
                    .unwrap_or(0);
                for check in &checks {
                    println!(
                        "{:<width$}  {:<10}  {}",
                        check.name, check.state, check.address
                    );
                }
            }
            Ok(if checks.iter().any(|check| check.state == "missing") {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Serve { listen } => {
            let home = remote::home()?;
            let identity = identity::Identity::load_or_create(&home)?;
            remote::serve(
                listen,
                remote::Server {
                    home,
                    identity,
                    status: Box::new(move || {
                        let config = config::Config::load()?;
                        serde_json::to_value(setup::read(&options, &config))
                            .map_err(|error| error.to_string())
                    }),
                },
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Focusrite(_)
        | Command::Id
        | Command::Pair { .. }
        | Command::Hosts { .. }
        | Command::Peers { .. } => {
            unreachable!("handled above")
        }
        Command::Route {
            ref receiver,
            ref source,
            clear: _,
            dry_run,
        } => {
            let outcome = route::run(&options, &config, receiver, source.as_deref(), dry_run)?;
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

/// Runs identity and pairing commands, which need no network options.
fn run_pairing(command: &Command) -> Result<Option<String>, String> {
    let home = || remote::home();
    Ok(Some(match command {
        Command::Id => pairing::id(&home()?)?,
        Command::Pair {
            address,
            alias,
            fingerprint,
            port,
            grant,
        } => pairing::pair(
            &home()?,
            alias.as_deref(),
            address,
            fingerprint.as_deref(),
            *port,
            role(*grant),
        )?,
        Command::Hosts { command } => match command {
            None => pairing::list_hosts(&home()?)?,
            Some(HostsCommand::Add {
                address,
                alias,
                fingerprint,
            }) => pairing::add_host(&home()?, alias.as_deref(), address, fingerprint.as_deref())?,
            Some(HostsCommand::Remove { name }) => pairing::remove_host(&home()?, name)?,
        },
        Command::Peers { command } => match command {
            None => pairing::list_peers(&home()?)?,
            Some(PeersCommand::Approve {
                selector,
                role: granted,
            }) => pairing::approve(&home()?, selector, role(*granted))?,
            Some(PeersCommand::Remove { selector }) => pairing::remove_peer(&home()?, selector)?,
        },
        _ => return Ok(None),
    }))
}

fn role(arg: RoleArg) -> trust::Role {
    match arg {
        RoleArg::Read => trust::Role::Read,
        RoleArg::Control => trust::Role::Control,
    }
}
