//! Builds the vendored No_FFT C++ codecs into a static library and links it.
//!
//! Options:
//!   NOVA_NOFFT_DIR   build against a different checkout instead of the
//!                    vendored copy in `no_fft` (e.g. a live
//!                    checkout of upstream `main`/trunk). Must contain
//!                    `nofft.cpp`, `acpcm.cpp` and their headers.

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let vendored = manifest.join("../../no_fft");

    let codec_dir = match env::var_os("NOVA_NOFFT_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => vendored.clone(),
    };
    println!("cargo:rerun-if-env-changed=NOVA_NOFFT_DIR");
    println!("cargo:rerun-if-changed={}", vendored.display());

    for file in ["nofft.cpp", "acpcm.cpp", "nofft.h", "acpcm.h"] {
        let path = codec_dir.join(file);
        if !path.exists() {
            panic!(
                "No_FFT sources are missing: {} does not exist.\n\
                 Run scripts/sync-nofft.sh to vendor them from upstream `main`,\n\
                 or set NOVA_NOFFT_DIR to a checkout that contains {file}.",
                path.display()
            );
        }
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let shim = manifest.join("shim/nova_nofft_shim.cpp");
    println!("cargo:rerun-if-changed={}", shim.display());

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .opt_level(3)
        // The codec's C reference build pins this so the recursive predictor
        // rounds identically everywhere; keep the same guarantee here.
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
