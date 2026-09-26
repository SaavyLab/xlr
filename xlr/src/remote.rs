//! The xlr wire protocol: one JSON request and one JSON response, each a
//! single line, per mutually authenticated TLS connection.

use crate::{
    identity::{Fingerprint, Identity, pairing_code},
    tls::{self, PinnedServer},
    trust::{Peers, Role},
};
use rustls::{ClientConnection, ServerConnection, StreamOwned, pki_types::ServerName};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

pub const DEFAULT_PORT: u16 = 7373;
const MAX_REQUEST: u64 = 64 * 1024;
const MAX_RESPONSE: u64 = 16 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const IO_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "method", rename_all = "kebab-case")]
pub enum Request {
    /// Introduces the client; answers with its role, or starts pairing.
    Hello {
        name: String,
        /// Set by `xlr pair`: the port the client itself serves on, so the
        /// host can add it back once approved.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        serve_port: Option<u16>,
    },
    /// This host's view of its hardware.
    Status,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Response {
    /// The answering host's own name (`[host] name`), on every response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RemoteError>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RemoteError {
    /// `pairing-required`, `forbidden`, `bad-request`, or `failed`.
    pub kind: String,
    pub message: String,
    /// The pairing code, with `pairing-required`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl Response {
    fn ok(result: Value) -> Self {
        Self {
            host: None,
            result: Some(result),
            error: None,
        }
    }

    fn error(kind: &str, message: impl Into<String>, code: Option<String>) -> Self {
        Self {
            host: None,
            result: None,
            error: Some(RemoteError {
                kind: kind.to_owned(),
                message: message.into(),
                code,
            }),
        }
    }
}

/// Adds the default port when `address` has none.
pub fn with_default_port(address: &str) -> String {
    let has_port = address.rsplit_once(':').is_some_and(|(host, port)| {
        !host.is_empty() && !host.ends_with(':') && port.parse::<u16>().is_ok()
    });
    if has_port {
        address.to_owned()
    } else {
        format!("{address}:{DEFAULT_PORT}")
    }
}

/// Sends one request. Returns the response and the fingerprint the host
/// presented (which equals `pinned` whenever `pinned` is given).
pub fn call(
    identity: &Identity,
    address: &str,
    pinned: Option<Fingerprint>,
    request: &Request,
) -> Result<(Response, Fingerprint), String> {
    let address = with_default_port(address);
    let socket = address
        .to_socket_addrs()
        .map_err(|error| format!("{address}: {error}"))?
        .next()
        .ok_or_else(|| format!("{address}: no address"))?;
    let tcp = TcpStream::connect_timeout(&socket, CONNECT_TIMEOUT)
        .map_err(|error| format!("{address}: {error}"))?;
    tcp.set_read_timeout(Some(IO_TIMEOUT)).ok();
    tcp.set_write_timeout(Some(IO_TIMEOUT)).ok();
    let verifier = PinnedServer::new(pinned);
    let config =
        tls::client_config(identity, verifier.clone()).map_err(|error| error.to_string())?;
    let connection =
        ClientConnection::new(config, ServerName::try_from("xlr").expect("valid name"))
            .map_err(|error| error.to_string())?;
    let mut stream = StreamOwned::new(connection, tcp);

    let fail = |error: io::Error| format!("{address}: {error}");
    let mut line = serde_json::to_vec(request).expect("requests serialize");
    line.push(b'\n');
    stream.write_all(&line).map_err(fail)?;
    stream.flush().map_err(fail)?;
    let mut body = Vec::new();
    match Read::by_ref(&mut stream)
        .take(MAX_RESPONSE)
        .read_to_end(&mut body)
    {
        Ok(_) => {}
        // A peer that closes without close_notify after a full response.
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof && !body.is_empty() => {}
        Err(error) => return Err(fail(error)),
    }
    let seen = verifier
        .seen()
        .ok_or_else(|| format!("{address}: no certificate presented"))?;
    let response = serde_json::from_slice(&body)
        .map_err(|error| format!("{address}: malformed response: {error}"))?;
    Ok((response, seen))
}

/// What a running server needs to answer requests.
pub struct Server {
    pub home: PathBuf,
    pub identity: Identity,
    /// Answers `Status`. Called with the local hardware lock held.
    pub status: Box<dyn Fn() -> Result<Value, String> + Send + Sync>,
}

/// Accepts connections forever, one thread per connection.
pub fn serve(listen: SocketAddr, server: Server) -> Result<(), String> {
    let listener = TcpListener::bind(listen).map_err(|error| format!("{listen}: {error}"))?;
    let config = tls::server_config(&server.identity).map_err(|error| error.to_string())?;
    let server = Arc::new(server);
    let hardware = Arc::new(Mutex::new(()));
    let peers_file = Arc::new(Mutex::new(()));
    eprintln!(
        "xlr serve: listening on {listen} as {}",
        server.identity.fingerprint.short()
    );
    for tcp in listener.incoming() {
        let Ok(tcp) = tcp else { continue };
        let (config, server, hardware, peers_file) = (
            config.clone(),
            server.clone(),
            hardware.clone(),
            peers_file.clone(),
        );
        thread::spawn(move || {
            let peer = tcp
                .peer_addr()
                .map_or_else(|_| "?".to_owned(), |addr| addr.to_string());
            if let Err(error) = handle(tcp, config, &server, &hardware, &peers_file) {
                eprintln!("xlr serve: {peer}: {error}");
            }
        });
    }
    Ok(())
}

fn handle(
    tcp: TcpStream,
    config: Arc<rustls::ServerConfig>,
    server: &Server,
    hardware: &Mutex<()>,
    peers_file: &Mutex<()>,
) -> Result<(), String> {
    let peer_ip = tcp.peer_addr().ok().map(|address| address.ip());
    tcp.set_read_timeout(Some(IO_TIMEOUT)).ok();
    tcp.set_write_timeout(Some(IO_TIMEOUT)).ok();
    let connection = ServerConnection::new(config).map_err(|error| error.to_string())?;
    let mut stream = StreamOwned::new(connection, tcp);
    while stream.conn.is_handshaking() {
        stream
            .conn
            .complete_io(&mut stream.sock)
            .map_err(|error| format!("handshake: {error}"))?;
    }
    let client = stream
        .conn
        .peer_certificates()
        .and_then(|certificates| certificates.first())
        .map(|certificate| Fingerprint::of(certificate))
        .ok_or("no client certificate")?;

    let mut line = String::new();
    BufReader::new(Read::by_ref(&mut stream).take(MAX_REQUEST))
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    let mut response = match serde_json::from_str::<Request>(&line) {
        Err(error) => Response::error("bad-request", error.to_string(), None),
        Ok(request) => respond(&request, &client, peer_ip, server, hardware, peers_file),
    };
    response.host = Some(hostname());
    eprintln!(
        "xlr serve: {} {} -> {}",
        client.short(),
        line.trim(),
        response
            .error
            .as_ref()
            .map_or("ok", |error| error.kind.as_str())
    );
    let mut body = serde_json::to_vec(&response).expect("responses serialize");
    body.push(b'\n');
    stream.write_all(&body).map_err(|error| error.to_string())?;
    stream.conn.send_close_notify();
    stream.flush().map_err(|error| error.to_string())
}

fn respond(
    request: &Request,
    client: &Fingerprint,
    peer_ip: Option<std::net::IpAddr>,
    server: &Server,
    hardware: &Mutex<()>,
    peers_file: &Mutex<()>,
) -> Response {
    let role = {
        let _guard = peers_file.lock().expect("not poisoned");
        let mut peers = match Peers::load(&server.home) {
            Ok(peers) => peers,
            Err(error) => return Response::error("failed", error, None),
        };
        match peers.role(client) {
            Some(role) => role,
            None => {
                let code = pairing_code(&server.identity.fingerprint, client);
                let (name, address) = match request {
                    Request::Hello { name, serve_port } => (
                        name.as_str(),
                        serve_port
                            .zip(peer_ip)
                            .map(|(port, ip)| SocketAddr::new(ip, port).to_string()),
                    ),
                    Request::Status => ("(unnamed)", None),
                };
                peers.request(client, name, code.clone(), address);
                if let Err(error) = peers.save(&server.home) {
                    return Response::error("failed", error, None);
                }
                return Response::error(
                    "pairing-required",
                    format!(
                        "not paired yet; approve on this host with `xlr peers approve {}`",
                        code.replace(' ', "")
                    ),
                    Some(code),
                );
            }
        }
    };
    match request {
        Request::Hello { .. } => Response::ok(serde_json::json!({ "role": role })),
        Request::Status => match role {
            Role::Read | Role::Control => {
                let _guard = hardware.lock().expect("not poisoned");
                match (server.status)() {
                    Ok(value) => Response::ok(value),
                    Err(error) => Response::error("failed", error, None),
                }
            }
        },
    }
}

/// This machine's name as other hosts will see it.
pub fn hostname() -> String {
    crate::config::host_name()
}

/// The xlr home, or an error explaining how to set one.
pub fn home() -> Result<PathBuf, String> {
    crate::config::home().ok_or_else(|| "cannot determine the xlr home; set XLR_HOME".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port_is_added_only_when_missing() {
        assert_eq!(with_default_port("mac-mini"), "mac-mini:7373");
        assert_eq!(with_default_port("10.0.0.2:9000"), "10.0.0.2:9000");
        assert_eq!(with_default_port("10.0.0.2"), "10.0.0.2:7373");
    }

    #[test]
    fn requests_have_a_stable_wire_shape() {
        assert_eq!(
            serde_json::to_string(&Request::Hello {
                name: "desk".to_owned(),
                serve_port: None
            })
            .unwrap(),
            r#"{"method":"hello","name":"desk"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Status).unwrap(),
            r#"{"method":"status"}"#
        );
    }

    #[test]
    fn pairing_then_approval_over_real_tls() {
        let base = std::env::temp_dir().join(format!("xlr-remote-test-{}", std::process::id()));
        let (server_home, client_home) = (base.join("server"), base.join("client"));
        let server_identity = Identity::load_or_create(&server_home).unwrap();
        let server_fingerprint = server_identity.fingerprint.clone();
        let client = Identity::load_or_create(&client_home).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let home = server_home.clone();
        thread::spawn(move || {
            serve(
                address,
                Server {
                    home,
                    identity: server_identity,
                    status: Box::new(|| Ok(serde_json::json!({ "hello": "world" }))),
                },
            )
        });
        let connect = |pinned: Option<Fingerprint>, request: Request| {
            for _ in 0..50 {
                match call(&client, &address.to_string(), pinned.clone(), &request) {
                    Err(error) if error.contains("refused") => {
                        thread::sleep(Duration::from_millis(20))
                    }
                    other => return other,
                }
            }
            panic!("server did not start");
        };

        let (first, seen) = connect(
            None,
            Request::Hello {
                name: "desk".to_owned(),
                serve_port: None,
            },
        )
        .unwrap();
        assert_eq!(seen, server_fingerprint);
        let error = first.error.expect("pairing required");
        assert_eq!(error.kind, "pairing-required");
        let code = error.code.unwrap();
        assert_eq!(code, pairing_code(&server_fingerprint, &client.fingerprint));

        let (denied, _) = connect(Some(seen.clone()), Request::Status).unwrap();
        assert_eq!(denied.error.unwrap().kind, "pairing-required");

        let mut peers = Peers::load(&server_home).unwrap();
        peers.approve(&code, Role::Read).unwrap();
        peers.save(&server_home).unwrap();
        let (status, _) = connect(Some(seen), Request::Status).unwrap();
        assert_eq!(status.result.unwrap()["hello"], "world");

        let wrong = Fingerprint::of(b"someone else");
        let mismatch = connect(Some(wrong), Request::Status).unwrap_err();
        assert!(mismatch.contains("is pinned"), "{mismatch}");
        std::fs::remove_dir_all(base).unwrap();
    }
}
