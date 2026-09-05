//! The dedicated server binary: the whole of it is in this crate's library, because the client
//! binary hosts the same thing under `noob_tube_client server`. This is what `deploy.sh` builds,
//! and it is the one that gets Bevy without its default features.

fn main() {
    noob_tube_server::run();
}
