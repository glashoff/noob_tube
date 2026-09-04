//! The TLS identity the game socket runs behind.
//!
//! WebTransport is QUIC, and QUIC has no unencrypted mode: there is no configuration of this game
//! in which the server does not present a certificate. See web.md §1 for why the transport is
//! WebTransport on native as well as in a browser.
//!
//! **Self-signed, for now.** A development server mints one at startup and publishes its SHA-256
//! digest over the metadata endpoint, which is what lets a client trust exactly this certificate
//! and nothing else: on native `wtransport` compares the digest, and in a browser the same hex
//! goes into `serverCertificateHashes`. That mechanism is the reason the identity is generated
//! here rather than by an operator — a hash nobody published is a hash nobody can pin.
//!
//! It is also why the validity is two weeks and not two years. A browser refuses a pinned
//! certificate valid for longer than fourteen days, so `Identity::self_signed` mints exactly that,
//! and a server left running past it stops being connectable — which is correct for a development
//! certificate and unacceptable for a deployment.
//!
//! **A deployment therefore needs a real one**, from a CA the browser already trusts, and then the
//! digest goes out empty and ordinary validation applies. That is not built: loading a PEM pair is
//! the deployment step of web.md §9, together with the reload a renewal every sixty days needs.
//! Until it exists, this server is a development server.

use bevy::prelude::*;
use lightyear::prelude::Identity;

/// The certificate the server presents, and the digest a client pins it by.
///
/// A resource because two Startup systems need it and neither owns it: the game socket takes the
/// identity, and the metadata endpoint publishes the digest. Building it in `main` rather than in
/// either of them keeps a failure to mint one at the top of the log instead of half way down.
#[derive(Resource)]
pub struct ServerCertificate {
    /// What the WebTransport endpoint presents.
    pub identity: Identity,
    /// Hex SHA-256 of it, no colons, as both a browser and `wtransport` want it — or empty when the
    /// certificate is publicly trusted and there is nothing to pin.
    pub digest: String,
}

impl ServerCertificate {
    /// Mints a development certificate, or explains why it could not.
    ///
    /// The names are what a self-signed certificate is *for* rather than what it is checked
    /// against: a client that pins the digest does not look at them at all. They are here so that
    /// a client which one day does not pin — because the server got a real certificate and this
    /// path is only the fallback — sees the names it dialled.
    pub fn self_signed(bind: std::net::SocketAddr) -> Result<Self, String> {
        let mut names = vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
        ];
        // The address the server was told to bind, when it is a real one. `0.0.0.0` is not a name
        // anybody dials and rcgen refuses it as a SAN.
        if !bind.ip().is_unspecified() && !bind.ip().is_loopback() {
            names.push(bind.ip().to_string());
        }

        let identity = Identity::self_signed(&names)
            .map_err(|error| format!("cannot mint a certificate for {names:?}: {error}"))?;
        let digest = hex(&identity);
        Ok(Self { identity, digest })
    }
}

/// The leaf certificate's SHA-256, lowercase hex and nothing else.
///
/// `wtransport` will format it dotted — `ab:cd:…` — which is what a human reading `openssl` output
/// expects and what neither of the two consumers accept: `WebTransportClientIo` parses plain hex,
/// and a browser wants raw bytes that the JavaScript side builds from the same string.
fn hex(identity: &Identity) -> String {
    let leaf = identity.certificate_chain().as_slice()[0].hash();
    AsRef::<[u8; 32]>::as_ref(&leaf)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
