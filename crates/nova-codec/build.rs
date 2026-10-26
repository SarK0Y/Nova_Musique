//! Builds the vendored No_FFT C++ codecs into a static library and links it.
//!
//! Automatically fetches upstream `main` on the first build when vendored
//! sources are absent. Set `NOVA_SYNC_NOFFT=1` to refresh the sources later.
//!
//! Options:
//!   NOVA_NOFFT_DIR    build against a different checkout instead of `no_fft`
//!   NOVA_SYNC_NOFFT   set to `1`/`true` to refresh upstream sources manually
//!   NOFFT_MODE        `release` (default, branch `main`) or `dev` (branch `trunk`)
//!   NOFFT_BRANCH      explicit upstream branch override
//!   NOFFT_REPO_URL    upstream repository URL override

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_REPO_URL: &str = "https://github.com/SarK0Y/No_FFT_v0.git";
const VENDORED_FILES: &[&str] = &["nofft.cpp", "nofft.h", "acpcm.cpp", "acpcm.h", "LICENSE"];

fn main() {
    let manifest =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set"));
    let vendored = manifest.join("../../no_fft");

    // Ensure the expected workspace-level directory exists before checking its
    // first-sync marker or trying to copy upstream files into it.
    fs::create_dir_all(&vendored).unwrap_or_else(|error| {
        panic!(
            "could not create No_FFT directory {}: {error}",
            vendored.display()
        )
    });

    println!("cargo:rerun-if-env-changed=NOVA_NOFFT_DIR");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_NOFFT");
    println!("cargo:rerun-if-env-changed=NOVA_SYNC_NOFFT");
    println!("cargo:rerun-if-env-changed=NOFFT_MODE");
    println!("cargo:rerun-if-env-changed=NOFFT_BRANCH");
    println!("cargo:rerun-if-env-changed=NOFFT_REPO_URL");
    println!("cargo:rerun-if-changed={}", vendored.display());

    // A marker records that the one-time first-build fetch has succeeded. Keep
    // it in the vendored directory so it travels with this checkout, rather
    // than repeating the network operation on every Cargo build.
    let first_sync_marker = vendored.join(".nova-nofft-initial-sync.done");
    println!("cargo:rerun-if-changed={}", first_sync_marker.display());

    let external_dir = env::var_os("NOVA_NOFFT_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let manual_sync_requested = env::var("NOVA_SYNC_NOFFT")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(false);
    // This build script always needs the No_FFT C++ sources to compile the
    // codec. Do not gate first-run synchronization on CARGO_FEATURE_NOFFT:
    // Cargo only sets that variable when this package's feature is enabled,
    // and the codec can be built through workspace/default-feature paths too.
    // Sync automatically on the first build, and also recover if the marker
    // exists but the required source files have since disappeared.
    let required_sources_missing = [
        "nofft.cpp",
        "acpcm.cpp",
        "nofft.h",
        "acpcm.h",
        "nfm.cpp",
        "nfm.h",
    ]
    .iter()
    .any(|file| !vendored.join(file).is_file());
    let first_build_sync_requested =
        external_dir.is_none() && (!first_sync_marker.is_file() || required_sources_missing);
    let sync_requested = manual_sync_requested || first_build_sync_requested;

    if sync_requested && external_dir.is_some() {
        panic!(
            "NOVA_SYNC_NOFFT and NOVA_NOFFT_DIR cannot be used together: \
             syncing updates the vendored no_fft/ directory, while NOVA_NOFFT_DIR \
             builds against an external checkout."
        );
    }

    if sync_requested {
        sync_nofft(&vendored);
        // Write the marker only after a complete successful sync. If fetching
        // or copying fails, the next build will try again.
        fs::write(&first_sync_marker, "Initial No_FFT sync completed\n").unwrap_or_else(|error| {
            panic!(
                "could not write first-sync marker {}: {error}",
                first_sync_marker.display()
            )
        });
    }

    let codec_dir = external_dir.unwrap_or_else(|| vendored.clone());
    for file in [
        "nofft.cpp",
        "acpcm.cpp",
        "nofft.h",
        "acpcm.h",
        "nfm.cpp",
        "nfm.h",
    ] {
        let path = codec_dir.join(file);
        if !path.is_file() {
            panic!(
                "No_FFT sources are missing: {} does not exist.\n\
                 Run `NOVA_SYNC_NOFFT=1 cargo build` to vendor them from upstream,\n\
                 or set NOVA_NOFFT_DIR to a checkout containing the required files.",
                path.display(),
            );
        }
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let shim = manifest.join("shim/nova_nofft_shim.cpp");
    if !shim.is_file() {
        panic!("No_FFT C++ shim is missing: {}", shim.display());
    }
    println!("cargo:rerun-if-changed={}", shim.display());

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .opt_level(3)
        // Match the codec's C reference build's floating-point behavior.
        .flag_if_supported("-ffp-contract=off")
        .include(&codec_dir)
        .include(codec_dir.join("third_party"))
        .file(codec_dir.join("nofft.cpp"))
        .file(codec_dir.join("acpcm.cpp"))
        .file(codec_dir.join("nfm.cpp"))
        .file(&shim)
        .warnings(false);
    build.compile("nova_nofft");

    // The codec uses libm; the C++ runtime is linked by the `cc` crate.
    println!("cargo:rustc-link-lib=m");
}

fn sync_nofft(destination: &Path) {
    let repo_url = env::var("NOFFT_REPO_URL").unwrap_or_else(|_| DEFAULT_REPO_URL.to_owned());
    let mode = env::var("NOFFT_MODE").unwrap_or_else(|_| "release".to_owned());
    let main_branch: String = "for_nova_musique_0.2.0".to_owned();
    let branch = match env::var("NOFFT_BRANCH") {
        Ok(branch) if !branch.trim().is_empty() => branch,
        _ => match mode.as_str() {
            "dev" => "trunk".to_owned(),
            "release" => main_branch,
            other => panic!("unknown NOFFT_MODE `{other}` (use `dev` or `release`)"),
        },
    };

    println!("cargo:warning=syncing No_FFT from {repo_url} (branch {branch}, mode {mode})");

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!("nova-sync-nofft-{}-{nonce}", std::process::id()));
    let checkout = temp_root.join("nofft");

    fs::create_dir_all(&temp_root).unwrap_or_else(|error| {
        panic!(
            "could not create temporary directory {}: {error}",
            temp_root.display()
        )
    });

    let clone_output = match Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            "--branch",
            branch.as_str(),
            repo_url.as_str(),
        ])
        .arg(&checkout)
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            let _ = fs::remove_dir_all(&temp_root);
            panic!("could not run `git clone`: {error}; ensure Git is installed");
        }
    };
    if !clone_output.status.success() {
        let detail = command_error("git clone", &clone_output);
        let _ = fs::remove_dir_all(&temp_root);
        panic!("failed to fetch No_FFT: {detail}");
    }

    let commit = git_output(&checkout, &["rev-parse", "HEAD"]);
    let date = git_output(&checkout, &["log", "-1", "--format=%cI"]);

    // Validate the codec sources on the requested branch before changing the
    // vendored copy. The current upstream `main` no longer ships the NFM
    // container (nfm.cpp/nfm.h), but Nova's stable C ABI still uses it. Keep
    // the requested codec branch as `main` and obtain only this compatibility
    // component from `trunk` when it is absent.
    for file in VENDORED_FILES {
        if !checkout.join(file).is_file() {
            let _ = fs::remove_dir_all(&temp_root);
            panic!("upstream checkout is missing required file `{file}`; vendored sources were not updated");
        }
    }
    // Copy the valid files from the requested branch as soon as they have
    // been validated. This way a later compatibility-fetch failure cannot
    // leave `no_fft/` empty after a successful fetch of upstream `main`.
    if !checkout.join("third_party/dr_wav.h").is_file() {
        let _ = fs::remove_dir_all(&temp_root);
        panic!("upstream checkout is missing `third_party/dr_wav.h`; vendored sources were not updated");
    }
    fs::create_dir_all(destination.join("third_party"))
        .unwrap_or_else(|error| panic!("could not create {}: {error}", destination.display()));
    for file in VENDORED_FILES {
        fs::copy(checkout.join(file), destination.join(file)).unwrap_or_else(|error| {
            panic!(
                "could not copy upstream file `{file}` into {}: {error}",
                destination.display()
            )
        });
    }
    fs::copy(
        checkout.join("third_party/dr_wav.h"),
        destination.join("third_party/dr_wav.h"),
    )
    .unwrap_or_else(|error| panic!("could not copy upstream `third_party/dr_wav.h`: {error}"));

    let nfm_compat = if checkout.join("nfm.cpp").is_file() && checkout.join("nfm.h").is_file() {
        None
    } else {
        let compat_checkout = temp_root.join("nfm-compat");
        let compat_output = Command::new("git")
            .args([
                "clone",
                "--depth",
                "1",
                "--branch",
                "trunk",
                repo_url.as_str(),
            ])
            .arg(&compat_checkout)
            .output()
            .unwrap_or_else(|error| {
                let _ = fs::remove_dir_all(&temp_root);
                panic!("could not run git clone for NFM compatibility files: {error}");
            });
        if !compat_output.status.success() {
            let detail =
                command_error("git clone (NFM compatibility branch trunk)", &compat_output);
            let _ = fs::remove_dir_all(&temp_root);
            panic!("upstream main does not contain nfm.cpp/nfm.h and compatibility fetch failed: {detail}");
        }
        for file in ["nfm.cpp", "nfm.h"] {
            if !compat_checkout.join(file).is_file() {
                let _ = fs::remove_dir_all(&temp_root);
                panic!("NFM compatibility checkout is missing `{file}`");
            }
        }
        Some(compat_checkout)
    };
    let nfm_source = nfm_compat.as_deref().unwrap_or(&checkout);
    for file in ["nfm.cpp", "nfm.h"] {
        fs::copy(nfm_source.join(file), destination.join(file)).unwrap_or_else(|error| {
            panic!(
                "could not copy NFM compatibility file `{file}` into {}: {error}",
                destination.display()
            )
        });
    }
    fs::copy(
        checkout.join("third_party/dr_wav.h"),
        destination.join("third_party/dr_wav.h"),
    )
    .unwrap_or_else(|error| panic!("could not copy upstream `third_party/dr_wav.h`: {error}"));

    let upstream = format!(
        "# Vendored: No_FFT codecs\n\n\
         This directory is a **verbatim, unmodified** copy of the codec sources from\n\n\
             {repo_url}\n\n\
         pinned to the `{branch}` branch ({mode} mode).\n\n\
         The NFM container is not present on the current upstream `main` branch.\n\
         When absent, `nfm.cpp` and `nfm.h` are fetched from the compatibility\n\
         branch `trunk`; the codec sources below remain from `{branch}`.\n\n\
         | Field | Value |\n| --- | --- |\n\
         | Repo | `{repo_url}` |\n\
         | Mode | `{mode}` |\n\
         | Branch | `{branch}` |\n\
         | Commit | `{commit}` |\n\
         | Date | `{date}` |\n\
         | NFM compatibility | `trunk` when absent from `{branch}` |\n\n\
         ## Vendored files\n\n\
         | File | Origin |\n| --- | --- |\n\
         | `nofft.cpp`, `nofft.h` | polynomial-fit codec (`.no_fft`) from `{branch}` |\n\
         | `acpcm.cpp`, `acpcm.h` | DPCM/basket codecs (`.nadc`, NFA1–NFA6) from `{branch}` |\n\
         | `nfm.cpp`, `nfm.h` | NFM container (metadata and cover art), compatibility branch `trunk` if absent |\n\
         | `third_party/dr_wav.h` | upstream WAV reader dependency |\n\
         | `LICENSE` | upstream licence |\n\n\
         Nothing here is edited by Nova: all glue lives in `crates/nova-codec`\n\
         (`shim/nova_nofft_shim.cpp` gives the C++ code a stable C ABI) and\n\
         `crates/nova-codec/build.rs`.\n\n\
         ## Refreshing\n\n\
             NOVA_SYNC_NOFFT=1 cargo build                       # refresh default branch main\n\
             NOFFT_MODE=dev NOVA_SYNC_NOFFT=1 cargo build         # refresh dev branch trunk\n\
             NOFFT_BRANCH=my-branch NOVA_SYNC_NOFFT=1 cargo build # explicit branch\n"
    );
    fs::write(destination.join("UPSTREAM.md"), upstream).unwrap_or_else(|error| {
        panic!(
            "could not write {}/UPSTREAM.md: {error}",
            destination.display()
        )
    });

    let _ = fs::remove_dir_all(&temp_root);
    println!("cargo:warning=vendored No_FFT {branch} @ {commit}");
}

fn git_output(checkout: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("could not run git {:?}: {error}", args));
    if !output.status.success() {
        panic!("{}", command_error("git", &output));
    }
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn command_error(command: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        format!("{command} exited with status {}", output.status)
    } else {
        format!("{command} exited with status {}: {stderr}", output.status)
    }
}
