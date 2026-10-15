// nova_nofft_shim.cpp — a small C ABI over the vendored No_FFT C++ codecs.
//
// The upstream headers (`nofft.h`, `acpcm.h`) use C++ linkage, which is not
// something Rust can bind to portably, and their `pcm_buf` ownership rules are
// implicit. This shim exposes a flat `extern "C"` surface that always fills
// caller-provided out-parameters and transfers PCM ownership through an
// explicit `nova_free` call.
//
// It is deliberately tiny: no codec logic lives here, only adapters. The
// vendored sources under no_fft stay byte-for-byte upstream.

#include "nofft.h"
#include "acpcm.h"

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <stdexcept>

// Ubuntu 20.04's libstdc++.so.6.0.28 (GCC 9.4) is missing the out-of-line
// std::__throw_bad_array_new_length() wrapper that <vector>'s fill-assign
// path references.  Provide it so the codec links everywhere.
namespace std {
void __throw_bad_array_new_length() {
    throw bad_array_new_length();
}
}

namespace {

// Reads the four magic bytes at the start of a file. Returns false on any
// short read or I/O failure, leaving `magic` untouched.
bool read_magic(const char* path, char magic[4]) {
    FILE* f = std::fopen(path, "rb");
    if (f == nullptr) {
        return false;
    }
    const size_t got = std::fread(magic, 1, 4, f);
    std::fclose(f);
    return got == 4;
}

bool magic_is(const char magic[4], const char* text) {
    return std::memcmp(magic, text, 4) == 0;
}

void read_u32le(const uint8_t* d, uint32_t* out) {
    *out = (uint32_t)d[0] | ((uint32_t)d[1] << 8) | ((uint32_t)d[2] << 16) |
           ((uint32_t)d[3] << 24);
}

void read_u16le(const uint8_t* d, uint16_t* out) {
    *out = (uint16_t)((uint16_t)d[0] | ((uint16_t)d[1] << 8));
}

}  // namespace

extern "C" {

// 0 for a polynomial `.no_fft` ("NOFF"), 1 for a `.nadc` (NFA1..NFA4), else -1.
int nova_codec_kind(const char* path) {
    char magic[4];
    if (!read_magic(path, magic)) {
        return -1;
    }
    if (magic_is(magic, NOFFT_MAGIC)) {
        return 0;
    }
    if (magic_is(magic, ACPCM_MAGIC) || magic_is(magic, ACPCM_MAGIC2) ||
        magic_is(magic, ACPCM_MAGIC3) || magic_is(magic, ACPCM_MAGIC4) ||
        magic_is(magic, ACPCM_MAGIC5) || magic_is(magic, ACPCM_MAGIC6)) {
        return 1;
    }
    return -1;
}

// Header-only probe. Works for both families and never decodes audio.
int nova_info(const char* path, uint32_t* out_rate, uint16_t* out_channels,
              uint64_t* out_frames) {
    if (out_rate == nullptr || out_channels == nullptr || out_frames == nullptr) {
        return (int)ERR_BAD_ARGS;
    }

    const int kind = nova_codec_kind(path);
    if (kind == 1) {
        acpcm_info info;
        const Err e = acpcm_file_info(path, &info);
        if (e != ERR_OK) {
            return (int)e;
        }
        *out_rate = info.sample_rate;
        *out_channels = (uint16_t)info.channels;
        *out_frames = info.frames;
        return (int)ERR_OK;
    }

    if (kind == 0) {
        FILE* f = std::fopen(path, "rb");
        if (f == nullptr) {
            return (int)ERR_OPEN;
        }
        uint8_t hdr[NOFFT_HEADER_SIZE];
        const size_t got = std::fread(hdr, 1, NOFFT_HEADER_SIZE, f);
        std::fclose(f);
        if (got != NOFFT_HEADER_SIZE) {
            return (int)ERR_IO;
        }
        uint32_t sr = 0, frame_len = 0, n_frames = 0, last_len = 0;
        uint16_t ch = 0;
        read_u32le(hdr + 8, &sr);
        read_u16le(hdr + 12, &ch);
        read_u32le(hdr + 16, &frame_len);
        read_u32le(hdr + 20, &n_frames);
        read_u32le(hdr + 24, &last_len);
        if (n_frames == 0) {
            return (int)ERR_BAD_NOFFT;
        }
        // Frame index n_frames-1 holds `last_len` frames; the rest hold frame_len.
        *out_rate = sr;
        *out_channels = ch;
        *out_frames = (uint64_t)(n_frames - 1) * (uint64_t)frame_len + (uint64_t)last_len;
        return (int)ERR_OK;
    }

    return (int)ERR_BAD_NOFFT;
}

// Decodes a whole file into a freshly malloc'd interleaved f32 buffer.
// On success the caller owns `*out_samples` and must release it with nova_free.
// On failure nothing is transferred (the shim frees its own buffer).
int nova_decode(const char* path, float** out_samples, size_t* out_count,
                uint32_t* out_rate, uint16_t* out_channels) {
    if (out_samples == nullptr || out_count == nullptr || out_rate == nullptr ||
        out_channels == nullptr) {
        return (int)ERR_BAD_ARGS;
    }

    const int kind = nova_codec_kind(path);
    if (kind < 0) {
        return (int)ERR_BAD_NOFFT;
    }

    pcm_buf buf;
    std::memset(&buf, 0, sizeof(buf));
    const Err e = (kind == 0) ? nofft_decode(path, &buf) : acpcm_decode(path, &buf);
    if (e != ERR_OK) {
        pcm_free(&buf);
        return (int)e;
    }
    if (buf.samples == nullptr && buf.count != 0) {
        pcm_free(&buf);
        return (int)ERR_NO_MEMORY;
    }

    *out_samples = buf.samples;
    *out_count = buf.count;
    *out_rate = buf.sample_rate;
    *out_channels = buf.channels;
    return (int)ERR_OK;
}

// Releases a buffer handed out by nova_decode.
void nova_free(float* samples) {
    if (samples == nullptr) {
        return;
    }
    pcm_buf buf;
    std::memset(&buf, 0, sizeof(buf));
    buf.samples = samples;
    pcm_free(&buf);
}

// Human readable text for an Err code, safe for NULL/unknown inputs.
const char* nova_err_str(int code) {
    if (code < 0 || code >= (int)ERR_COUNT || g_err_str == nullptr) {
        return "unknown codec error";
    }
    const char* text = g_err_str((Err)code);
    return text != nullptr ? text : "unknown codec error";
}

// --- test/conversion helpers (not used on the playback path) ---------------

// Writes a deterministic tone-and-chirp WAV via the codec's own generator.
int nova_gen_wav(const char* path, double seconds, uint32_t channels, uint32_t rate) {
    return (int)acpcm_gen(path, seconds, channels, rate);
}

// Encodes a WAV to `.no_fft` (polynomial fit, f16 coefficients, least squares).
int nova_encode_nofft(const char* in_wav, const char* out_path, uint32_t frame_len,
                      uint16_t degree) {
    pcm_buf p;
    std::memset(&p, 0, sizeof(p));
    Err e = wav_load(in_wav, &p);
    if (e != ERR_OK) {
        return (int)e;
    }
    e = nofft_encode(out_path, &p, frame_len, degree, NOFFT_COEF_F16, NOFFT_FIT_LEAST_SQ);
    pcm_free(&p);
    return (int)e;
}

// Encodes a WAV to `.nadc`. `variant` selects the codec: 1 = NFA1, 2 = NFA2
// (block 1024, drift policy), 3 = NFA3, 4 = NFA4 (170 baskets), 5 = NFA5
// (multiplicative log-domain predictor), 6 = NFA6 (blocked NFA5).
int nova_encode_nadc(const char* in_wav, const char* out_path, uint16_t bits,
                     uint32_t variant) {
    pcm_buf p;
    std::memset(&p, 0, sizeof(p));
    Err e = wav_load(in_wav, &p);
    if (e != ERR_OK) {
        return (int)e;
    }
    switch (variant) {
        case 1:
            e = acpcm_encode(out_path, &p, bits);
            break;
        case 2:
            e = acpcm_encode2(out_path, &p, bits, 1024, ACP2_DRIFT);
            break;
        case 3:
            e = acpcm_encode3(out_path, &p, bits);
            break;
        case 4:
            e = acpcm_encode4_baskets(out_path, &p, bits, 170);
            break;
        case 5:
            e = acpcm_encode5(out_path, &p, bits, 2.0, 0.01);
            break;
        case 6:
            e = acpcm_encode6(out_path, &p, bits, 1024, 2.0, 2.6);
            break;
        default:
            e = ERR_BAD_ARGS;
            break;
    }
    pcm_free(&p);
    return (int)e;
}

}  // extern "C"
