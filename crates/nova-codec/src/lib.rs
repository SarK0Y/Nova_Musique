//! Safe Rust bindings to the vendored **No_FFT** codecs.
//!
//! Nova links the upstream C++ codec sources
//! ([`SarK0Y/No_FFT_v0`](https://github.com/SarK0Y/No_FFT_v0), the `main`
//! branch) into the binary via `build.rs` and talks to them through a tiny C
//! ABI shim (`shim/nova_nofft_shim.cpp`). This crate is the only place that
//! touches that `unsafe` surface; everything above it is plain Rust.
//!
//! Two codec families live in the container:
//!
//! - [`CodecKind::Polynomial`] — magic `NOFF`, the variable-degree polynomial
//!   fit written to `.no_fft` files;
//! - [`CodecKind::Nfa`] — magic `NFA1`..`NFA4`, the DPCM/basket family written
//!   to `.nadc` files.
//!
//! Detection is by the four magic bytes, not the file name, so a renamed file
//! still decodes. [`info`] reads only the header (cheap enough for a library
//! scan) and [`decode`] produces interleaved `f32` PCM ready for playback.

use std::ffi::{c_int, CStr, CString};
use std::fmt;
use std::path::Path;
use std::time::Duration;

/// File extensions handled by the polynomial codec.
pub const NOFFT_EXTENSIONS: &[&str] = &["no_fft"];
/// File extensions handled by the NFA (DPCM/basket) codecs.
pub const NADC_EXTENSIONS: &[&str] = &["nadc"];
/// Every extension the vendored codecs can decode, lower-cased, no dot.
pub const CODEC_EXTENSIONS: &[&str] = &["no_fft", "nadc"];

mod ffi {
    //! The C ABI exported by `shim/nova_nofft_shim.cpp`.
    use std::ffi::{c_char, c_int};

    extern "C" {
        pub fn nova_codec_kind(path: *const c_char) -> c_int;
        pub fn nova_info(
            path: *const c_char,
            rate: *mut u32,
            channels: *mut u16,
            frames: *mut u64,
        ) -> c_int;
        pub fn nova_decode(
            path: *const c_char,
            samples: *mut *mut f32,
            count: *mut usize,
            rate: *mut u32,
            channels: *mut u16,
        ) -> c_int;
        pub fn nova_free(samples: *mut f32);
        pub fn nova_err_str(code: c_int) -> *const c_char;

        pub fn nova_gen_wav(
            path: *const c_char,
            seconds: f64,
            channels: u32,
            rate: u32,
        ) -> c_int;
        pub fn nova_encode_nofft(
            in_wav: *const c_char,
            out: *const c_char,
            frame_len: u32,
            degree: u16,
        ) -> c_int;
        pub fn nova_encode_nadc(
            in_wav: *const c_char,
            out: *const c_char,
            bits: u16,
            variant: u32,
        ) -> c_int;
    }
}

/// Which No_FFT family a file belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecKind {
    /// Variable-degree polynomial fit, magic `NOFF` (`.no_fft`).
    Polynomial,
    /// DPCM/basket family, magic `NFA1`..`NFA4` (`.nadc`).
    Nfa,
}

impl CodecKind {
    fn from_raw(raw: c_int) -> Option<Self> {
        match raw {
            0 => Some(CodecKind::Polynomial),
            1 => Some(CodecKind::Nfa),
            _ => None,
        }
    }
}

/// Which NFA variant to write when encoding (decode handles all of them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NadcVariant {
    /// Classic DPCM with a fixed predictor.
    Nfa1,
    /// Forward-adaptive predictor, refit every 1024 frames (drift policy).
    Nfa2,
    /// Residual baskets.
    Nfa3,
    /// Basket-only, no predictor (170 baskets).
    Nfa4,
    /// Multiplicative (log-domain) predictor, lossy.
    Nfa5,
    /// NFA5 with a per-block re-fit of the log step, lossy.
    Nfa6,
}

impl NadcVariant {
    fn as_raw(self) -> u32 {
        match self {
            NadcVariant::Nfa1 => 1,
            NadcVariant::Nfa2 => 2,
            NadcVariant::Nfa3 => 3,
            NadcVariant::Nfa4 => 4,
            NadcVariant::Nfa5 => 5,
            NadcVariant::Nfa6 => 6,
        }
    }
}

/// A failure reported by the codec layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecError {
    code: i32,
    message: String,
}

impl CodecError {
    /// The raw `Err` code from the codec, or `-1` for a Nova-side failure.
    pub fn code(&self) -> i32 {
        self.code
    }
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CodecError {}

/// Stream properties read from a codec header, without decoding any audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecInfo {
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// Channel count (interleaved).
    pub channels: u16,
    /// Frames (one sample per channel) in the stream.
    pub frames: u64,
}

impl CodecInfo {
    /// Playback length implied by the header.
    pub fn duration(&self) -> Duration {
        if self.sample_rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(self.frames as f64 / f64::from(self.sample_rate))
    }
}

/// Decoded audio: interleaved `f32` samples plus their format.
#[derive(Debug, Clone, PartialEq)]
pub struct Pcm {
    /// Interleaved samples, `channels` values per frame.
    pub samples: Vec<f32>,
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u16,
}

impl Pcm {
    /// Number of frames held in [`Pcm::samples`].
    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }

    /// Playback length of the decoded audio.
    pub fn duration(&self) -> Duration {
        if self.sample_rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(self.frames() as f64 / f64::from(self.sample_rate))
    }
}

/// Identifies a file by its codec magic bytes.
///
/// Returns `None` for anything that is not an `NOFF`/`NFA*` file — including
/// unreadable paths, which are simply "not a codec file" here.
pub fn probe(path: &Path) -> Option<CodecKind> {
    let c_path = c_path(path).ok()?;
    let raw = unsafe { ffi::nova_codec_kind(c_path.as_ptr()) };
    CodecKind::from_raw(raw)
}

/// Whether `path` is a file the vendored decoders can play.
pub fn is_codec_file(path: &Path) -> bool {
    probe(path).is_some()
}

/// Reads only the header of a codec file.
pub fn info(path: &Path) -> Result<CodecInfo, CodecError> {
    let c_path = c_path(path)?;
    let mut rate = 0u32;
    let mut channels = 0u16;
    let mut frames = 0u64;
    let code = unsafe {
        ffi::nova_info(
            c_path.as_ptr(),
            &mut rate,
            &mut channels,
            &mut frames,
        )
    };
    check(code)?;
    Ok(CodecInfo {
        sample_rate: rate,
        channels,
        frames,
    })
}

/// Decodes a whole codec file into interleaved `f32` PCM.
pub fn decode(path: &Path) -> Result<Pcm, CodecError> {
    let c_path = c_path(path)?;
    let mut samples: *mut f32 = std::ptr::null_mut();
    let mut count = 0usize;
    let mut rate = 0u32;
    let mut channels = 0u16;
    let code = unsafe {
        ffi::nova_decode(
            c_path.as_ptr(),
            &mut samples,
            &mut count,
            &mut rate,
            &mut channels,
        )
    };
    check(code)?;

    // The codec allocates with malloc and hands ownership over; copy into a
    // Rust Vec before releasing it, so both allocators stay in their lane.
    let data = if samples.is_null() || count == 0 {
        Vec::new()
    } else {
        // SAFETY: on success the shim guarantees `samples` points to `count`
        // initialised f32 values that remain valid until `nova_free`.
        unsafe { std::slice::from_raw_parts(samples, count).to_vec() }
    };
    unsafe { ffi::nova_free(samples) };

    Ok(Pcm {
        samples: data,
        sample_rate: rate,
        channels,
    })
}

/// Writes a deterministic tone-and-chirp WAV using the codec's own generator.
///
/// Intended for tests and demos: it lets the encode/decode path be exercised
/// without shipping any binary fixtures.
pub fn generate_wav(
    path: &Path,
    seconds: f64,
    channels: u32,
    sample_rate: u32,
) -> Result<(), CodecError> {
    let out = c_path(path)?;
    let code = unsafe { ffi::nova_gen_wav(out.as_ptr(), seconds, channels, sample_rate) };
    check(code)
}

/// Encodes a WAV file to the polynomial `.no_fft` format.
pub fn encode_nofft(
    wav: &Path,
    out: &Path,
    frame_len: u32,
    degree: u16,
) -> Result<(), CodecError> {
    let input = c_path(wav)?;
    let output = c_path(out)?;
    let code = unsafe { ffi::nova_encode_nofft(input.as_ptr(), output.as_ptr(), frame_len, degree) };
    check(code)
}

/// Encodes a WAV file to the NFA (`.nadc`) format.
pub fn encode_nadc(
    wav: &Path,
    out: &Path,
    bits: u16,
    variant: NadcVariant,
) -> Result<(), CodecError> {
    let input = c_path(wav)?;
    let output = c_path(out)?;
    let code = unsafe {
        ffi::nova_encode_nadc(input.as_ptr(), output.as_ptr(), bits, variant.as_raw())
    };
    check(code)
}

fn check(code: i32) -> Result<(), CodecError> {
    if code == 0 {
        return Ok(());
    }
    let message = {
        let ptr = unsafe { ffi::nova_err_str(code) };
        if ptr.is_null() {
            format!("codec error {code}")
        } else {
            unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
        }
    };
    Err(CodecError { code, message })
}

/// Turns a path into a NUL-terminated C string, preserving non-UTF-8 bytes on
/// Unix.
fn c_path(path: &Path) -> Result<CString, CodecError> {
    CString::new(path_bytes(path)).map_err(|_| CodecError {
        code: -1,
        message: format!("path contains a NUL byte: {}", path.display()),
    })
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path as StdPath, PathBuf};

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nova-codec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A 0.25 s stereo 44.1 kHz test signal written as WAV.
    fn test_wav(dir: &StdPath) -> PathBuf {
        let wav = dir.join("signal.wav");
        generate_wav(&wav, 0.25, 2, 44_100).expect("generate test wav");
        wav
    }

    fn assert_playable(pcm: &Pcm, expected_frames: u64) {
        assert_eq!(pcm.channels, 2);
        assert_eq!(pcm.sample_rate, 44_100);
        assert_eq!(pcm.frames() as u64, expected_frames);
        assert!(pcm.samples.iter().all(|s| s.is_finite()));
        assert!(
            pcm.samples.iter().any(|s| s.abs() > 1e-4),
            "decoded audio is silent"
        );
    }

    #[test]
    fn polynomial_round_trip_probes_and_decodes() {
        let dir = tmpdir("poly");
        let wav = test_wav(&dir);
        let out = dir.join("signal.poly.no_fft");

        encode_nofft(&wav, &out, 256, 8).expect("encode");
        assert_eq!(probe(&out), Some(CodecKind::Polynomial));

        let head = info(&out).expect("header info");
        assert_eq!(head.channels, 2);
        assert_eq!(head.sample_rate, 44_100);
        assert!(head.frames > 0);

        let pcm = decode(&out).expect("decode");
        assert_playable(&pcm, head.frames);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_nadc_variant_round_trips() {
        let dir = tmpdir("nadc");
        let wav = test_wav(&dir);

        for variant in [
            NadcVariant::Nfa1,
            NadcVariant::Nfa2,
            NadcVariant::Nfa3,
            NadcVariant::Nfa4,
            NadcVariant::Nfa5,
            NadcVariant::Nfa6,
        ] {
            let out = dir.join(format!("signal.{}.nadc", variant.as_raw()));
            encode_nadc(&wav, &out, 6, variant).expect("encode");
            assert_eq!(probe(&out), Some(CodecKind::Nfa));

            let head = info(&out).expect("header info");
            assert_eq!(head.channels, 2);
            assert_eq!(head.sample_rate, 44_100);

            let pcm = decode(&out).expect("decode");
            assert_playable(&pcm, head.frames);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_codec_files_are_not_probed() {
        let dir = tmpdir("junk");
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"just some text").expect("write");

        assert_eq!(probe(&file), None);
        assert!(!is_codec_file(&file));
        assert!(info(&file).is_err());
        assert!(decode(&file).is_err());
        // A missing file is likewise "not a codec file", not a panic.
        assert_eq!(probe(&dir.join("nope.no_fft")), None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
