//! What a client needs to know *before* it can connect.
//!
//! **The server owns every setting the two ends have to agree on**, and this is how a client is
//! told: see [`NetConfig::adopt_from_server`](crate::tuning::NetConfig::adopt_from_server), which
//! is the list. All of them have to be settled before `App::new` — the tick rate goes into the
//! lightyear plugin group and into `Time<Fixed>`, and the link conditioner goes onto the transport
//! — so none of them can be learned from the connection itself.
//!
//! Nor can it be learned afterwards. Lightyear 0.29 never puts the tick duration on the wire:
//! `SenderMetadata` carries the send interval *in ticks*, which is circular. And its
//! `SetTickDuration` trigger is half-finished — the only global observer updates `Time<Fixed>` and
//! leaves the `TickDuration` resource that every timeline converts with untouched.
//!
//! So this is a second, tiny listener beside the game socket: HTTP on TCP, one endpoint, returning
//! a [`ServerInfo`] as JSON. A client fetches it in `main`, before `App::new`.
//!
//! **JSON rather than the TOML the config file speaks**: in a browser this is
//! fetched by JavaScript before the wasm module starts, because nothing on that side may block. A
//! browser parses JSON for free and would need a shipped parser for anything else.
//!
//! For the tick rate it is a convenience rather than a safety net: that one is baked into the
//! netcode protocol id and rejects a mismatched peer whether or not this endpoint was reachable.
//! For the rest there is no net at all. A client and a server simulating different links, or
//! disagreeing about lag compensation, connect perfectly happily and produce a session whose
//! numbers mean nothing — which is exactly the failure this endpoint now exists to prevent, and
//! why a client that could not reach it says so in as many words.
//!
//! Two of the three fields are not settings at all but facts about the running server, and they
//! are here because a browser cannot discover them for itself: it has no name resolver to turn a
//! host into the address a connect token must name, and no way to learn a self-signed
//! certificate's digest. See [`ServerInfo`].

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use crate::tuning::NetConfig;

/// Everything a client is told before it builds its app.
///
/// [`net`](Self::net) is the settings, and the only part with a rule about who owns what. The other
/// two are facts about this particular process:
///
/// - [`token_addr`](Self::token_addr) is the address a netcode connect token has to name. The
///   client mints its own token — there is no backend issuing them — so it has to know what the
///   server will accept, and netcode compares it against the address the server actually bound.
///   Working it out on the client meant resolving a host name, which a browser cannot do at all.
/// - [`cert_digest`](Self::cert_digest) is the SHA-256 of a self-signed certificate, hex, no
///   colons — empty when the server holds a real one and ordinary validation applies.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ServerInfo {
    /// The settings the server owns.
    pub net: NetConfig,
    /// What a connect token must name for this server to accept it.
    pub token_addr: SocketAddr,
    /// Hex SHA-256 of the server's certificate, or empty when it has a publicly trusted one.
    pub cert_digest: String,
}

/// How long a client waits for the endpoint before giving up and using its own settings.
///
/// Short on purpose: this is on the startup path, and a server without a metadata endpoint should
/// cost a moment, not a timeout.
const FETCH_TIMEOUT: Duration = Duration::from_millis(500);

/// Serves the config until the process ends.
///
/// Runs on its own thread rather than in a Bevy system: it is blocking I/O that must not share a
/// schedule with a fixed timestep, and it answers questions from processes that have not connected
/// yet, so it has nothing to do with the ECS.
///
/// A failure to bind is logged and otherwise ignored. The endpoint is a convenience; a server that
/// cannot offer it should still serve the game.
pub fn serve(info: ServerInfo, port: u16) {
    // `[::]` rather than `0.0.0.0`, and it accepts IPv4 as well — the same choice, for the same
    // reason, as the game socket's; see `bind_address` in the server, where the reason is written
    // down. A client that reaches this endpoint over one family and the game over another would be
    // a confusing way to fail.
    let addr = SocketAddr::new(std::net::Ipv6Addr::UNSPECIFIED.into(), port);
    let listener = match TcpListener::bind(addr) {
        Ok(listener) => listener,
        Err(err) => {
            warn!("no metadata endpoint on {addr}: {err}");
            return;
        }
    };
    let body = match serde_json::to_string(&info) {
        Ok(body) => body,
        Err(err) => {
            warn!("cannot serialise the config: {err}");
            return;
        }
    };
    info!("metadata endpoint on http://{addr}/");

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => answer(stream, &body),
                // One bad accept is not a reason to stop answering.
                Err(err) => debug!("metadata connection failed: {err}"),
            }
        }
    });
}

/// Answers one request, whatever it asked for.
///
/// There is one thing to say, so there is no routing and no method check: any request gets the
/// config. Hand-written HTTP is only defensible because the response never varies.
///
/// `Access-Control-Allow-Origin` is not here and should not be: the browser reads this through the
/// same origin that served the page, proxied to this port on loopback. A page on
/// another origin has no business minting connect tokens for this server.
fn answer(mut stream: TcpStream, body: &str) {
    // The request has to be drained before replying, or a client that is still writing gets its
    // connection reset instead of the answer. One read is enough for a request this small.
    let _ = stream.set_read_timeout(Some(FETCH_TIMEOUT));
    let mut scratch = [0u8; 1024];
    let _ = stream.read(&mut scratch);

    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len(),
    );
    let _ = stream.write_all(response.as_bytes());
}

/// Asks a server what it is running, or `None` if it will not say.
///
/// Unreachable is not an error: plenty of servers will not have this, and the protocol id still
/// refuses a mismatched connection. The caller logs what it decided.
pub fn fetch(addr: SocketAddr) -> Option<ServerInfo> {
    let mut stream = TcpStream::connect_timeout(&addr, FETCH_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(FETCH_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(FETCH_TIMEOUT)).ok()?;
    // HTTP/1.0 so the server closes when it is done and the read below ends on its own.
    stream.write_all(b"GET / HTTP/1.0\r\n\r\n").ok()?;

    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    // Headers and body are separated by a blank line; everything after it is the JSON.
    let body = response.split_once("\r\n\r\n").map(|(_, body)| body)?;
    serde_json::from_str(body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The endpoint's own round trip, over a real socket on a port the OS picked.
    ///
    /// It exists because the body is hand-written HTTP: a header that is one byte wrong is
    /// invisible in review and fatal at startup, and the only thing that can say which it is, is a
    /// client parsing what the server actually wrote.
    #[test]
    fn what_the_server_wrote_is_what_the_client_reads() {
        let mut net = NetConfig::default();
        net.tick_hz = 32.0;
        let info = ServerInfo {
            net,
            token_addr: "10.0.0.1:5555".parse().expect("an address"),
            cert_digest: "ab".repeat(32),
        };

        // Port zero, then ask the OS which one it gave us — a fixed port would collide with
        // whatever else is running on the machine the tests run on.
        let port = TcpListener::bind(("127.0.0.1", 0))
            .and_then(|probe| probe.local_addr())
            .expect("a free port")
            .port();
        serve(info.clone(), port);

        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        assert_eq!(fetch(addr), Some(info));
    }
}
