// Installs Nova Musique at build time: download -> copy its README -> build & install -> erase.
use std::env;
use std::fs;
use std::path::Path;

#[path = "src/install.rs"]
#[allow(dead_code)] // REPO and install() are for the app binary; the build script needs download/erase
mod install;

fn main() {
if env::var_os("CARGO_FEATURE_INSTALL").is_none() {
    println!(
        "cargo:warning=Dear User, use feature install to get fresh version of Nova Musique. :)"
    );
    return;
	}
    // shortcut: the install reruns only when the install logic changes; touch build.rs (or cargo
    // clean) to reinstall, otherwise a plain rebuild does nothing
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/install.rs");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_INSTALL");

    // the whole install only runs on demand: cargo build --features install
    if env::var_os("CARGO_FEATURE_INSTALL").is_none() {
        return;
    }

    // a stale ./nova from an interrupted run would push the clone into /tmp instead;
    // best effort — download() falls back to /tmp if this fails
    let stale = Path::new(env!("CARGO_MANIFEST_DIR")).join("nova");
    if stale.exists() {
        let _ = fs::remove_dir_all(&stale);
    }

    let src = install::download().unwrap_or_else(|e| panic!("{e}"));
    let readme = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    if let Err(e) = fs::copy(src.join("README.md"), &readme) {
        panic!(
            "{} has no README.md: {e}; downloaded sources kept at {}",
            install::REPO,
            src.display()
        );
    }
    install::build_and_install(&src).unwrap_or_else(|e| panic!("{e}"));
    install::erase(&src).unwrap_or_else(|e| panic!("{e}"));
}
