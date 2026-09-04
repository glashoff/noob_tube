//! Which assets are here, written down for the target that cannot look.
//!
//! Three places in the client choose between two things by asking whether a file exists —
//! [`Kit::present`](../src/character.rs), the clip substitution beside it, and the vehicle's model
//! or box. All three ask the *filesystem* rather than the asset server, and the reason is in each
//! of their comments: the asset server answers some frames later, and by then the choice has been
//! made and built around.
//!
//! In a browser there is no filesystem to ask. `std::path::Path::exists` compiles there and answers
//! `false` to everything, which would not be an error — it would be a client that silently plays
//! the fallback character and the box-shaped vehicle, for ever, on every machine.
//!
//! So the answer is worked out here, where there *is* a filesystem, and baked into the build. The
//! same script builds the bundle and publishes the directory this walks (`web/build.sh`), so the
//! list and the files served are the same decision made once.
//!
//! Native builds do not read this. They keep asking the disk, because on a desktop the disk is the
//! truth and can change between two runs of the same binary.

use std::path::Path;

fn main() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets");
    // A file added or taken away changes the answer, so the build has to notice. Directory
    // granularity is all cargo offers, and all this needs.
    println!("cargo:rerun-if-changed={}", assets.display());

    let mut found = Vec::new();
    walk(&assets, &assets, &mut found);
    found.sort();

    let out = Path::new(&std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"))
        .join("shipped_assets.txt");
    std::fs::write(&out, found.join("\n")).expect("the list of assets could not be written");
}

/// Every file under `at`, named the way the asset server names it: relative, with forward slashes.
fn walk(root: &Path, at: &Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(at) else {
        // A checkout with no assets at all is a build with an empty list, not a failed build: the
        // three callers all have a fallback, and this is exactly the case they have it for.
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, found);
        } else if let Ok(relative) = path.strip_prefix(root) {
            found.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}
