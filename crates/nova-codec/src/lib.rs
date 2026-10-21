//! Safe Rust bindings to the vendored **No_FFT** codecs.
//!
//! Nova links the upstream C++ codec sources
//! ([`SarK0Y/No_FFT_v0`](https://github.com/SarK0Y/No_FFT_v0), the `main`
//! branch) into the binary via `build.rs` and talks to them through a tiny C
//! ABI shim (`shim/nova_nofft_shim.cpp`). This crate is the only place that
//! touches that `unsafe` surface; everything above it is plain Rust.
//!
//! Three container families are supported:
//!
//! - [`CodecKind::Polynomial`] — magic `NOFF`, the variable-degree polynomial
//!   fit written to `.no_fft` files;
//! - [`CodecKind::Nfa`] — magic `NFA1`..`NFA6`, the DPCM/basket family written
//!   to `.nadc` files;
//! - [`CodecKind::Nfm`] — magic `nfm`, a container that wraps any of the above
//!   codecs with optional text metadata and a cover image (`.nfm` files).
//!
//! Detection is by the magic bytes, not the file name, so a renamed file
//! still decodes. [`info`] reads only the header (cheap enough for a library
//! scan) and [`decode`] produces interleaved `f32` PCM ready for playback.
//! NFM files are unwrapped transparently: [`nfm_read`] extracts the inner
//! payload, metadata and cover image. Both the current container layout and the
//! older v1 layout are accepted.

use std::ffi::{c_char, c_int, CStr, CString};
use std::fmt;
use std::path::Path;
use std::time::Duration;

/// File extensions handled by the polynomial codec.
pub const NOFFT_EXTENSIONS: &[&str] = &["no_fft"];
/// File extensions handled by the NFA (DPCM/basket) codecs.
pub const NADC_EXTENSIONS: &[&str] = &["nadc"];
/// File extension handled by the NFM container.
pub const NFM_EXTENSIONS: &[&str] = &["nfm"];
/// NFM cover image type: no image.
pub const NFM_IMG_NONE: u8 = 0;
/// NFM cover image type: PNG.
pub const NFM_IMG_PNG: u8 = 1;
/// NFM cover image type: JPEG.
pub const NFM_IMG_JPEG: u8 = 2;
/// Every extension the vendored codecs can decode, lower-cased, no dot.
pub const CODEC_EXTENSIONS: &[&str] = &["no_fft", "nadc"];

mod ffi {
    //! The C ABI exported by `shim/nova_nofft_shim.cpp`.
    use std::ffi::{c_char, c_int};

    extern "C" {
        pub fn nova_codec_kind(path: *const c_char) -> c_int;
        pub fn nova_nfm_read(
            path: *const c_char,
            payload: *mut *mut u8,
            payload_len: *mut usize,
            title: *mut *const c_char,
            artist: *mut *const c_char,
            album: *mut *const c_char,
            img: *mut *mut u8,
            img_len: *mut usize,
            img_type: *mut c_int,
        ) -> c_int;
        pub fn nova_nfm_free_meta(
            title: *mut *const c_char,
            artist: *mut *const c_char,
            album: *mut *const c_char,
        );
        pub fn nova_nfm_write(
            path: *const c_char,
            payload: *const u8,
            payload_len: usize,
            title: *const c_char,
            artist: *const c_char,
            album: *const c_char,
            img: *const u8,
            img_len: usize,
            img_type: c_int,
        ) -> c_int;
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

        pub fn nova_gen_wav(path: *const c_char, seconds: f64, channels: u32, rate: u32) -> c_int;
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
        pub fn nova_encode_nadc5_rate(
            in_wav: *const c_char,
            out: *const c_char,
            bits: u16,
            rate: f64,
        ) -> c_int;
    }
}

/// Which No_FFT family a file belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecKind {
    /// Variable-degree polynomial fit, magic `NOFF` (`.no_fft`).
    Polynomial,
    /// DPCM/basket family, magic `NFA1`..`NFA6` (`.nadc`).
    Nfa,
    /// NFM container, magic `nfm`, wrapping any of the above (`.nfm`).
    Nfm,
}

impl CodecKind {
    fn from_raw(raw: c_int) -> Option<Self> {
        match raw {
            0 => Some(CodecKind::Polynomial),
            1 => Some(CodecKind::Nfa),
            2 => Some(CodecKind::Nfm),
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
    let code = unsafe { ffi::nova_info(c_path.as_ptr(), &mut rate, &mut channels, &mut frames) };
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

/// Text metadata extracted from an NFM container.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NfmMeta {
    /// Track title, if present.
    pub title: Option<String>,
    /// Artist name, if present.
    pub artist: Option<String>,
    /// Album name, if present.
    pub album: Option<String>,
}

/// Cover image extracted from an NFM container.
#[derive(Debug, Clone, PartialEq)]
pub struct NfmCover {
    /// Raw image bytes (PNG or JPEG).
    pub data: Vec<u8>,
    /// Image type: 1 = PNG, 2 = JPEG.
    pub img_type: u8,
}

/// Result of reading an NFM container: inner payload + metadata + cover.
#[derive(Debug)]
pub struct NfmFile {
    /// Inner codec payload bytes (the raw `.no_fft` or `.nadc` stream).
    pub payload: Vec<u8>,
    /// Text metadata from the container.
    pub meta: NfmMeta,
    /// Cover image, if present.
    pub cover: Option<NfmCover>,
}

/// Reads an NFM container, extracting the inner codec payload, text metadata
/// and cover image.
///
/// Two on-disk layouts exist:
///
/// * **v2** (current) — the header always opens with an image block
///   (`img_type u8`, `img_len u32`, image), then text fields. This is what the
///   vendored `nfm_read` parses.
/// * **v1** (legacy) — the header holds text fields followed by an optional
///   image, with no image block; an empty header (`header_len == 0`) means no
///   metadata at all. The vendored reader rejects this, so it is parsed here as
///   a compatibility fallback.
pub fn nfm_read(path: &Path) -> Result<NfmFile, CodecError> {
    match nfm_read_current(path) {
        Ok(file) => Ok(file),
        Err(err) => {
            let data = match std::fs::read(path) {
                Ok(data) => data,
                Err(_) => return Err(err),
            };
            match nfm_read_legacy(&data) {
                Some(file) => Ok(file),
                None => Err(err),
            }
        }
    }
}

/// Reads the current (v2) NFM layout through the vendored C++ reader.
fn nfm_read_current(path: &Path) -> Result<NfmFile, CodecError> {
    let c_path = c_path(path)?;
    let mut payload: *mut u8 = std::ptr::null_mut();
    let mut payload_len = 0usize;
    let mut title: *const c_char = std::ptr::null();
    let mut artist: *const c_char = std::ptr::null();
    let mut album: *const c_char = std::ptr::null();
    let mut img: *mut u8 = std::ptr::null_mut();
    let mut img_len = 0usize;
    let mut img_type: c_int = 0;

    let code = unsafe {
        ffi::nova_nfm_read(
            c_path.as_ptr(),
            &mut payload,
            &mut payload_len,
            &mut title,
            &mut artist,
            &mut album,
            &mut img,
            &mut img_len,
            &mut img_type,
        )
    };
    check(code)?;

    // Copy payload into a Rust Vec.
    let payload_data = if payload.is_null() || payload_len == 0 {
        Vec::new()
    } else {
        // SAFETY: on success the shim guarantees `payload` points to `payload_len`
        // initialised u8 values that remain valid until `nova_free`.
        unsafe { std::slice::from_raw_parts(payload, payload_len).to_vec() }
    };
    unsafe { ffi::nova_free(payload as *mut f32) };

    // Copy image into a Rust Vec.
    let cover = if img.is_null() || img_len == 0 {
        None
    } else {
        // SAFETY: on success the shim guarantees `img` points to `img_len`
        // initialised u8 values that remain valid until `nova_free`.
        let img_data = unsafe { std::slice::from_raw_parts(img, img_len).to_vec() };
        unsafe { ffi::nova_free(img as *mut f32) };
        Some(NfmCover {
            data: img_data,
            img_type: img_type as u8,
        })
    };

    // Copy meta strings into Rust Strings.
    let meta = NfmMeta {
        title: copy_meta_string(title),
        artist: copy_meta_string(artist),
        album: copy_meta_string(album),
    };
    unsafe { ffi::nova_nfm_free_meta(&mut title, &mut artist, &mut album) };

    Ok(NfmFile {
        payload: payload_data,
        meta,
        cover,
    })
}

/// Parses the legacy (v1) NFM layout from a whole-file buffer.
///
/// v1 stores the header as text fields followed by an *optional* image block
/// (`img_type u8`, `img_len u32`, image); v2 moved the image block to the front
/// and made it mandatory. An empty header (`header_len == 0`) carries neither
/// fields nor image. Returns `None` when the buffer is not a valid v1 container
/// or when the trailing payload does not look like a known codec stream.
fn nfm_read_legacy(data: &[u8]) -> Option<NfmFile> {
    if data.len() < 11 || &data[0..3] != b"nfm" {
        return None;
    }
    let hlen = u64::from_le_bytes(data[3..11].try_into().ok()?) as usize;
    let hend = 11usize.checked_add(hlen)?;
    if hend > data.len() {
        return None;
    }

    // Text fields come first, with no leading image block.
    let mut title = None;
    let mut artist = None;
    let mut album = None;
    let mut pos = 11usize;
    while pos < hend {
        if pos + 2 > hend {
            break;
        }
        let name_len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        let name_start = pos + 2;
        let value_len_start = name_start + name_len;
        let value_start = value_len_start + 4;
        if value_start > hend {
            break;
        }
        let value_len =
            u32::from_le_bytes(data[value_len_start..value_start].try_into().ok()?) as usize;
        let value_end = value_start.checked_add(value_len)?;
        if value_end > hend {
            break;
        }
        let name = &data[name_start..value_len_start];
        let value = String::from_utf8_lossy(&data[value_start..value_end]).into_owned();
        match name {
            b"title" if title.is_none() => title = Some(value),
            b"artist" if artist.is_none() => artist = Some(value),
            b"album" if album.is_none() => album = Some(value),
            _ => {}
        }
        pos = value_end;
    }

    // The optional image block trails the fields.
    let mut cover = None;
    if pos + 5 <= hend {
        let img_type = data[pos];
        let img_len = u32::from_le_bytes(data[pos + 1..pos + 5].try_into().ok()?) as usize;
        let end = pos.checked_add(5 + img_len)?;
        if (img_type == NFM_IMG_PNG || img_type == NFM_IMG_JPEG) && img_len > 0 && end <= hend {
            cover = Some(NfmCover {
                data: data[pos + 5..end].to_vec(),
                img_type,
            });
        }
    }
    // Fallback for writers that emitted a bare image without the block header.
    if cover.is_none() {
        let mut i = 11usize;
        while i < hend {
            let is_png = i + 4 <= hend && &data[i..i + 4] == b"\x89PNG";
            let is_jpeg = i + 2 <= hend && data[i] == 0xFF && data[i + 1] == 0xD8;
            if is_png || is_jpeg {
                cover = Some(NfmCover {
                    data: data[i..hend].to_vec(),
                    img_type: if is_png { NFM_IMG_PNG } else { NFM_IMG_JPEG },
                });
                break;
            }
            i += 1;
        }
    }

    let payload = data[hend..].to_vec();
    // A valid container must still carry a decodable codec payload.
    kind_from_bytes(&payload).ok()?;

    Some(NfmFile {
        payload,
        meta: NfmMeta {
            title,
            artist,
            album,
        },
        cover,
    })
}

fn copy_meta_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the shim guarantees a valid NUL-terminated C string.
    let s = unsafe { CStr::from_ptr(ptr) };
    Some(s.to_string_lossy().into_owned())
}

/// Writes an NFM container wrapping `payload`.
///
/// Any of the text fields or the cover image may be omitted.  `img_type` is
/// `1` for PNG and `2` for JPEG (see [`NFM_IMG_PNG`] / [`NFM_IMG_JPEG`]).
pub fn nfm_write(
    path: &Path,
    payload: &[u8],
    meta: &NfmMeta,
    cover: Option<&NfmCover>,
) -> Result<(), CodecError> {
    let c_path = c_path(path)?;
    let title = opt_c_string(meta.title.as_deref())?;
    let artist = opt_c_string(meta.artist.as_deref())?;
    let album = opt_c_string(meta.album.as_deref())?;
    let (img_ptr, img_len, img_type) = match cover {
        Some(c) => (c.data.as_ptr(), c.data.len(), c.img_type as c_int),
        None => (std::ptr::null(), 0, 0),
    };

    let code = unsafe {
        ffi::nova_nfm_write(
            c_path.as_ptr(),
            payload.as_ptr(),
            payload.len(),
            title.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            artist.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            album.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            img_ptr,
            img_len,
            img_type,
        )
    };
    check(code)
}

fn opt_c_string(value: Option<&str>) -> Result<Option<CString>, CodecError> {
    match value {
        Some(s) => CString::new(s).map(Some).map_err(|_| CodecError {
            code: -1,
            message: "metadata contains a NUL byte".to_string(),
        }),
        None => Ok(None),
    }
}

/// Detects codec kind from file magic bytes in a buffer.
pub fn kind_from_bytes(buf: &[u8]) -> Result<CodecKind, CodecError> {
    if buf.len() < 4 {
        return Err(CodecError {
            code: -1,
            message: "file too small".to_string(),
        });
    }
    // NOFF
    if &buf[0..4] == b"NOFF" {
        return Ok(CodecKind::Polynomial);
    }
    // NFA1..NFA6
    if &buf[0..3] == b"NFA" && (b'1'..=b'6').contains(&buf[3]) {
        return Ok(CodecKind::Nfa);
    }
    // nfm
    if &buf[0..3] == b"nfm" {
        return Ok(CodecKind::Nfm);
    }
    Err(CodecError {
        code: -1,
        message: "unknown magic".to_string(),
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
pub fn encode_nofft(wav: &Path, out: &Path, frame_len: u32, degree: u16) -> Result<(), CodecError> {
    let input = c_path(wav)?;
    let output = c_path(out)?;
    let code =
        unsafe { ffi::nova_encode_nofft(input.as_ptr(), output.as_ptr(), frame_len, degree) };
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
    let code =
        unsafe { ffi::nova_encode_nadc(input.as_ptr(), output.as_ptr(), bits, variant.as_raw()) };
    check(code)
}

/// Encodes a WAV to rate-controlled NFA5: the output is plain NFA5, but the
/// encoder auto-dials each chunk's error to hold `rate` bits per sample (quiet
/// material stays fine and cheap, loud material coarsens to budget).  `rate`
/// must be finite and > 0.
pub fn encode_nadc5_rate(wav: &Path, out: &Path, bits: u16, rate: f64) -> Result<(), CodecError> {
    let input = c_path(wav)?;
    let output = c_path(out)?;
    let code = unsafe { ffi::nova_encode_nadc5_rate(input.as_ptr(), output.as_ptr(), bits, rate) };
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
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned()
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
    fn nfm_container_round_trips() {
        let dir = tmpdir("nfm");
        let wav = test_wav(&dir);

        // Wrap a polynomial payload and an NFA payload, with metadata and a
        // tiny (fake) PNG cover, then read them back.
        let poly = dir.join("signal.poly.no_fft");
        encode_nofft(&wav, &poly, 256, 8).expect("encode poly");
        let nfa = dir.join("signal.nfa1.nadc");
        encode_nadc(&wav, &nfa, 6, NadcVariant::Nfa1).expect("encode nfa");

        let meta = NfmMeta {
            title: Some("Round Trip".to_string()),
            artist: Some("Nova".to_string()),
            album: None,
        };
        let cover = NfmCover {
            data: {
                let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
                png.extend_from_slice(b"not-a-real-png");
                png
            },
            img_type: NFM_IMG_PNG,
        };

        for (name, payload_path) in [("poly.nfm", &poly), ("nfa.nfm", &nfa)] {
            let payload = std::fs::read(payload_path).expect("read payload");
            let nfm = dir.join(name);
            nfm_write(&nfm, &payload, &meta, Some(&cover)).expect("nfm_write");

            assert_eq!(probe(&nfm), Some(CodecKind::Nfm));

            let read = nfm_read(&nfm).expect("nfm_read");
            assert_eq!(read.payload, payload);
            assert_eq!(read.meta, meta);
            let got_cover = read.cover.expect("cover present");
            assert_eq!(got_cover.img_type, NFM_IMG_PNG);
            assert_eq!(got_cover.data, cover.data);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Builds a legacy (v1) NFM file: magic + header_len + header + payload,
    /// where the header is text fields followed by an optional image block
    /// (`img_type u8`, `img_len u32`, image) — no leading image block.
    fn write_legacy_nfm(
        path: &StdPath,
        fields: &[(&str, &str)],
        image: Option<&[u8]>,
        payload: &[u8],
    ) {
        use std::io::Write as _;
        let mut header: Vec<u8> = Vec::new();
        for (name, value) in fields {
            header.extend_from_slice(&(name.len() as u16).to_le_bytes());
            header.extend_from_slice(name.as_bytes());
            header.extend_from_slice(&(value.len() as u32).to_le_bytes());
            header.extend_from_slice(value.as_bytes());
        }
        if let Some(image) = image {
            header.push(NFM_IMG_PNG);
            header.extend_from_slice(&(image.len() as u32).to_le_bytes());
            header.extend_from_slice(image);
        }

        let mut out: Vec<u8> = b"nfm".to_vec();
        out.extend_from_slice(&(header.len() as u64).to_le_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(payload);

        let mut file = std::fs::File::create(path).expect("create legacy nfm");
        file.write_all(&out).expect("write legacy nfm");
    }

    #[test]
    fn legacy_nfm_files_without_image_block_are_read() {
        let dir = tmpdir("nfm_legacy");
        let wav = test_wav(&dir);
        let nfa = dir.join("signal.nfa1.nadc");
        encode_nadc(&wav, &nfa, 6, NadcVariant::Nfa1).expect("encode");
        let payload = std::fs::read(&nfa).expect("read payload");

        // Empty header: no metadata, no cover (the layout that `tst.nfm` uses).
        let bare = dir.join("bare.nfm");
        write_legacy_nfm(&bare, &[], None, &payload);
        let read = nfm_read(&bare).expect("read bare legacy");
        assert_eq!(read.payload, payload);
        assert_eq!(read.meta, NfmMeta::default());
        assert!(read.cover.is_none());

        // With text fields and a trailing PNG image.
        let png = {
            let mut p = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
            p.extend_from_slice(b"legacy-image");
            p
        };
        let tagged = dir.join("tagged.nfm");
        write_legacy_nfm(
            &tagged,
            &[("title", "Legacy"), ("artist", "Old Format")],
            Some(&png),
            &payload,
        );
        let read = nfm_read(&tagged).expect("read tagged legacy");
        assert_eq!(read.payload, payload);
        assert_eq!(read.meta.title.as_deref(), Some("Legacy"));
        assert_eq!(read.meta.artist.as_deref(), Some("Old Format"));
        let cover = read.cover.expect("legacy cover");
        assert_eq!(cover.img_type, NFM_IMG_PNG);
        assert_eq!(cover.data, png);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nfm_without_metadata_or_cover_round_trips() {
        let dir = tmpdir("nfm_bare");
        let wav = test_wav(&dir);
        let nfa = dir.join("signal.nfa2.nadc");
        encode_nadc(&wav, &nfa, 6, NadcVariant::Nfa2).expect("encode");

        let payload = std::fs::read(&nfa).expect("read");
        let nfm = dir.join("bare.nfm");
        nfm_write(&nfm, &payload, &NfmMeta::default(), None).expect("nfm_write");

        let read = nfm_read(&nfm).expect("nfm_read");
        assert_eq!(read.payload, payload);
        assert_eq!(read.meta, NfmMeta::default());
        assert!(read.cover.is_none());

        // The inner payload still probes and decodes as its own codec.
        let kind = kind_from_bytes(&read.payload).expect("inner kind");
        assert_eq!(kind, CodecKind::Nfa);

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
