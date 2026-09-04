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
//! the server's [`NetConfig`] as TOML — the same language the config file speaks, parsed by the
//! same code. A client fetches it in `main`, before `App::new`.
//!
//! For the tick rate it is a convenience rather than a safety net: that one is baked into the
//! netcode protocol id and rejects a mismatched peer whether or not this endpoint was reachable.
//! For the rest there is no net at all. A client and a server simulating different links, or
//! disagreeing about lag compensation, connect perfectly happily and produce a session whose
//! numbers mean nothing — which is exactly the failure this endpoint now exists to prevent, and
//! why a client that could not reach it says so in as many words.

use tracing::{debug, info, warn};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use crate::tuning::NetConfig;

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
pub fn serve(config: NetConfig, port: u16) {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = match TcpListener::bind(addr) {
        Ok(listener) => listener,
        Err(err) => {
            warn!("no metadata endpoint on {addr}: {err}");
            return;
        }
    };
    let body = match toml::to_string(&config) {
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
fn answer(mut stream: TcpStream, body: &str) {
    // The request has to be drained before replying, or a client that is still writing gets its
    // connection reset instead of the answer. One read is enough for a request this small.
    let _ = stream.set_read_timeout(Some(FETCH_TIMEOUT));
    let mut scratch = [0u8; 1024];
    let _ = stream.read(&mut scratch);

    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
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
pub fn fetch(addr: SocketAddr) -> Option<NetConfig> {
    let mut stream = TcpStream::connect_timeout(&addr, FETCH_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(FETCH_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(FETCH_TIMEOUT)).ok()?;
    // HTTP/1.0 so the server closes when it is done and the read below ends on its own.
    stream.write_all(b"GET / HTTP/1.0\r\n\r\n").ok()?;

    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    // Headers and body are separated by a blank line; everything after it is the TOML.
    let body = response.split_once("\r\n\r\n").map(|(_, body)| body)?;
    toml::from_str(body).ok()
}
