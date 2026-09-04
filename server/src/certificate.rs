//! The TLS identity the game socket runs behind.
//!
//! WebTransport is QUIC, and QUIC has no unencrypted mode: there is no configuration of this game
//! in which the server does not present a certificate. See web.md §1 for why the transport is
//! WebTransport on native as well as in a browser.
//!
//! **Two shapes, and which one is in use decides what a client has to be told.**
//!
//! A deployment is handed a certificate from an authority the browser already trusts —
//! `NOOB_TUBE_CERT` and `NOOB_TUBE_KEY`, in the environment beside `NOOB_TUBE_BIND`, because these
//! are facts about a machine rather than settings of a game. Then there is nothing to publish: the
//! digest goes out empty and validation happens the ordinary way.
//!
//! Everything else mints one at startup and publishes its SHA-256 digest over the metadata
//! endpoint, which is what lets a client trust exactly this certificate and nothing else: on native
//! `wtransport` compares the digest, and in a browser the same hex goes into
//! `serverCertificateHashes`. That is also why the generated one lasts a fortnight and not a year
//! — a browser refuses a pinned certificate valid for longer than fourteen days — and why a server
//! left running past it stops being connectable. Correct for a development certificate,
//! unacceptable for a deployment, which is the whole reason the first shape exists.
//!
//! The two cannot be mixed. A publicly trusted certificate is good for sixty or ninety days, so
//! publishing *its* digest would ask the browser to pin something it refuses to pin, and every
//! connection would fail. Supplying the files means saying "this one needs no pinning".
//!
//! What is not here yet is the renewal: a certificate replaced every sixty days is a server that
//! has to be told, and today that means restarting it. See web.md §8.

use bevy::prelude::*;
use lightyear::prelude::Identity;
use wtransport::tls::{Certificate, CertificateChain, PrivateKey};

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
    /// The certificate an operator supplied, or one minted for this run.
    ///
    /// A supplied pair that cannot be read is fatal rather than quietly falling back to a generated
    /// one. The fallback would start, would look like it worked, and would refuse every browser
    /// that arrived — which is a worse failure than not starting, and one nobody would connect to
    /// the file they had just installed.
    pub fn load(bind: std::net::SocketAddr) -> Result<Self, String> {
        match (std::env::var_os("NOOB_TUBE_CERT"), std::env::var_os("NOOB_TUBE_KEY")) {
            (Some(chain), Some(key)) => Self::from_pemfiles(chain.as_ref(), key.as_ref()),
            (None, None) => Self::self_signed(bind),
            // One without the other is a half-finished deployment, and guessing which half was
            // meant is how a server ends up serving a certificate nobody chose.
            _ => Err("NOOB_TUBE_CERT and NOOB_TUBE_KEY have to be given together".to_string()),
        }
    }

    /// A certificate and key an authority issued, in the PEM files certbot leaves behind.
    ///
    /// `fullchain.pem` rather than `cert.pem`: a browser needs the intermediates, and a chain with
    /// only the leaf in it fails on some clients and not others, which is the worst way for this to
    /// go wrong.
    fn from_pemfiles(chain: &std::path::Path, key: &std::path::Path) -> Result<Self, String> {
        let pem = std::fs::read(chain).map_err(|why| format!("{}: {why}", chain.display()))?;
        let certificates = rustls_pemfile::certs(&mut &pem[..])
            .map(|found| {
                found
                    .map_err(|why| format!("{}: {why}", chain.display()))
                    .and_then(|der| {
                        Certificate::from_der(der.to_vec())
                            .map_err(|why| format!("{}: {why}", chain.display()))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if certificates.is_empty() {
            return Err(format!("{} holds no certificate", chain.display()));
        }

        let pem = std::fs::read(key).map_err(|why| format!("{}: {why}", key.display()))?;
        // PKCS#8 only — `BEGIN PRIVATE KEY`, which is what every current tool writes and what
        // `from_der_pkcs8` below is willing to read. The older formats are still out there, and a
        // line saying which one this is and how to convert it beats "invalid key".
        let private = rustls_pemfile::pkcs8_private_keys(&mut &pem[..])
            .next()
            .ok_or_else(|| {
                format!(
                    "{} holds no PKCS#8 private key (`BEGIN PRIVATE KEY`). If it is an older \
                     format: openssl pkcs8 -topk8 -nocrypt -in {} -out {}.pkcs8",
                    key.display(),
                    key.display(),
                    key.display(),
                )
            })?
            .map_err(|why| format!("{}: {why}", key.display()))?;

        Ok(Self {
            identity: Identity::new(
                CertificateChain::new(certificates),
                PrivateKey::from_der_pkcs8(private.secret_pkcs8_der().to_vec()),
            ),
            // Nothing to pin: this one is trusted on its own account, and asking a browser to pin
            // it would fail on the validity period alone. See this module's header.
            digest: String::new(),
        })
    }

    /// Mints a development certificate, or explains why it could not.
    ///
    /// The names are what a self-signed certificate is *for* rather than what it is checked
    /// against: a client that pins the digest does not look at them at all. They are here so that
    /// a client which one day does not pin — because the server got a real certificate and this
    /// path is only the fallback — sees the names it dialled.
    fn self_signed(bind: std::net::SocketAddr) -> Result<Self, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("noob_tube_{name}_{}.pem", std::process::id()))
    }

    /// A PEM pair written and read back is the certificate that was written.
    ///
    /// The point is not the round trip — it is the two decisions either side of it. A supplied
    /// certificate must come back with an **empty digest**, because publishing one would ask a
    /// browser to pin a certificate that outlives what pinning allows, and every connection would
    /// fail with nothing pointing at this file. And the key has to survive being written as
    /// PKCS#8 and parsed back, which is the one part of this that is somebody else's format.
    #[test]
    fn a_supplied_certificate_is_taken_as_it_is_and_pinned_by_nobody() {
        let minted = ServerCertificate::self_signed("127.0.0.1:0".parse().expect("an address"))
            .expect("a certificate could not be minted");
        let leaf = &minted.identity.certificate_chain().as_slice()[0];

        let chain_path = scratch("chain");
        let key_path = scratch("key");
        std::fs::write(&chain_path, leaf.to_pem()).expect("the scratch chain");
        std::fs::write(&key_path, minted.identity.private_key().to_secret_pem())
            .expect("the scratch key");

        let loaded = ServerCertificate::from_pemfiles(&chain_path, &key_path)
            .expect("the pair could not be read back");
        assert_eq!(hex(&loaded.identity), hex(&minted.identity), "a different certificate");
        assert!(loaded.digest.is_empty(), "a supplied certificate was published for pinning");

        let _ = std::fs::remove_file(&chain_path);
        let _ = std::fs::remove_file(&key_path);
    }

    /// A key in a format this cannot read says which format it wanted and how to get there.
    ///
    /// The failure it replaces is a server that will not start with "invalid key" and no idea
    /// which of the four things in a PEM file is wrong.
    #[test]
    fn a_key_in_the_wrong_format_says_so() {
        let chain_path = scratch("lonely_chain");
        let key_path = scratch("sec1");
        let minted = ServerCertificate::self_signed("127.0.0.1:0".parse().expect("an address"))
            .expect("a certificate could not be minted");
        std::fs::write(&chain_path, minted.identity.certificate_chain().as_slice()[0].to_pem())
            .expect("the scratch chain");
        // A PEM file with a section this does not take. The bytes need not be a real key: nothing
        // gets as far as looking at them.
        std::fs::write(&key_path, "-----BEGIN EC PRIVATE KEY-----\nMHQ=\n-----END EC PRIVATE KEY-----\n")
            .expect("the scratch key");

        let trouble = match ServerCertificate::from_pemfiles(&chain_path, &key_path) {
            Err(trouble) => trouble,
            Ok(_) => panic!("a key this cannot read was accepted"),
        };
        assert!(trouble.contains("PKCS#8"), "the error does not say what it wanted: {trouble}");
        assert!(trouble.contains("openssl"), "the error does not say how to get there: {trouble}");

        let _ = std::fs::remove_file(&chain_path);
        let _ = std::fs::remove_file(&key_path);
    }
}
