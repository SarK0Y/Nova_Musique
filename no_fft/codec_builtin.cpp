/* codec_builtin.cpp - the codecs that ship inside the core library.
 *
 * Each of these is a nofft_codec vtable.  A host can use them directly through
 * nofft_codec_builtin(), or load the matching module (which just hands the same
 * vtable back) through the loader in codec.cpp.  The encode side reads only the
 * options the caller flagged, so a host that sets nothing gets the same defaults
 * the command line tool uses.
 */

#include "codec.h"
#include "acpcm.h"

#include <cstdlib>
#include <cstring>

namespace {

uint16_t opt_bits(const nofft_codec_opts* o)
{
    return (o != NULL && (o->flags & NOFFT_OPT_BITS)) ? o->bits : 6;
}

uint32_t opt_block(const nofft_codec_opts* o, uint32_t dflt)
{
    uint32_t b = (o != NULL && (o->flags & NOFFT_OPT_BLOCK)) ? o->block_len : dflt;
    return (b == 0) ? dflt : b;
}

double opt_e(const nofft_codec_opts* o)
{
    return (o != NULL && (o->flags & NOFFT_OPT_E)) ? o->e : ACPCM5_E_DEFAULT;
}

double opt_err(const nofft_codec_opts* o)
{
    return (o != NULL && (o->flags & NOFFT_OPT_ERR)) ? o->err : ACPCM5_ERR_DEFAULT;
}

double opt_rate(const nofft_codec_opts* o)
{
    return (o != NULL && (o->flags & NOFFT_OPT_RATE)) ? o->rate : ACPCM6_RATE_DEFAULT;
}

/* ---- poly (the .no_fft polynomial codec) ---- */

Err poly_encode(const pcm_buf* in, const nofft_codec_opts* o,
                uint8_t** out, size_t* out_len)
{
    uint32_t frame_len = (o != NULL && (o->flags & NOFFT_OPT_FRAME_LEN))
                             ? o->frame_len : 20;
    uint16_t degree = (o != NULL && (o->flags & NOFFT_OPT_DEGREE))
                          ? o->degree : 3;
    nofft_coeff_format cf = (o != NULL && (o->flags & NOFFT_OPT_COEF))
                                ? (nofft_coeff_format)o->coef_format
                                : NOFFT_COEF_F32;
    nofft_fit fit = (o != NULL && (o->flags & NOFFT_OPT_FIT))
                        ? (nofft_fit)o->fit : NOFFT_FIT_INTERP;
    return nofft_encode_buf(out, out_len, in, frame_len, degree, cf, fit);
}

Err poly_decode(const uint8_t* in, size_t in_len, pcm_buf* out)
{
    return nofft_decode_buf(in, in_len, out);
}

/* The .no_fft header is host endian and version dependent (see nofft.h), so it
   is parsed here rather than through a shared helper the writer does not
   expose.  Only the fields that describe the stream are read. */
Err poly_info(const uint8_t* in, size_t in_len, nofft_codec_info* out)
{
    if (in == NULL || out == NULL)
        return ERR_BAD_ARGS;
    if (in_len < NOFFT_HEADER_SIZE || std::memcmp(in, NOFFT_MAGIC, 4) != 0)
        return ERR_BAD_NOFFT;

    uint32_t version, rate, frame_len, n_frames, last_len;
    uint16_t ch, degree;
    std::memcpy(&version, in + 4, 4);
    std::memcpy(&rate, in + 8, 4);
    std::memcpy(&ch, in + 12, 2);
    std::memcpy(&degree, in + 14, 2);
    std::memcpy(&frame_len, in + 16, 4);
    std::memcpy(&n_frames, in + 20, 4);
    std::memcpy(&last_len, in + 24, 4);

    if (version < 2 || version > NOFFT_VERSION)
        return ERR_BAD_NOFFT;
    if (ch == 0 || frame_len == 0 || n_frames == 0 || last_len == 0 ||
        last_len > frame_len)
        return ERR_BAD_NOFFT;

    out->sample_rate = rate;
    out->channels = ch;
    out->frames = (uint64_t)(n_frames - 1) * frame_len + last_len;
    out->payload = (in_len > NOFFT_HEADER_SIZE)
                       ? (uint64_t)(in_len - NOFFT_HEADER_SIZE) : 0;
    out->bits = degree;
    out->format = 0;
    return ERR_OK;
}

/* ---- acpcm (NFA1..NFA6) ---- */

Err nfa1_encode(const pcm_buf* in, const nofft_codec_opts* o,
                uint8_t** out, size_t* out_len)
{
    return acpcm_encode_buf(out, out_len, in, opt_bits(o));
}

Err nfa2_encode(const pcm_buf* in, const nofft_codec_opts* o,
                uint8_t** out, size_t* out_len)
{
    acpcm_p2_policy policy = (o != NULL && (o->flags & NOFFT_OPT_POLICY))
                                 ? (acpcm_p2_policy)o->policy : ACP2_DRIFT;
    return acpcm_encode2_buf(out, out_len, in, opt_bits(o),
                             opt_block(o, 1024), policy);
}

Err nfa3_encode(const pcm_buf* in, const nofft_codec_opts* o,
                uint8_t** out, size_t* out_len)
{
    return acpcm_encode3_buf(out, out_len, in, opt_bits(o));
}

Err nfa4_encode(const pcm_buf* in, const nofft_codec_opts* o,
                uint8_t** out, size_t* out_len)
{
    if (o != NULL && (o->flags & NOFFT_OPT_BASKETS))
        return acpcm_encode4_baskets_buf(out, out_len, in, opt_bits(o), o->baskets);
    uint32_t step = (o != NULL && (o->flags & NOFFT_OPT_STEP)) ? o->step : 5;
    return acpcm_encode4_buf(out, out_len, in, opt_bits(o), step);
}

Err nfa5_encode(const pcm_buf* in, const nofft_codec_opts* o,
                uint8_t** out, size_t* out_len)
{
    if (o != NULL && (o->flags & NOFFT_OPT_AUTO))
        return acpcm_encode5_auto_buf(out, out_len, in, opt_bits(o),
                                      opt_e(o), opt_rate(o));
    return acpcm_encode5_buf(out, out_len, in, opt_bits(o), opt_e(o), opt_err(o));
}

Err nfa6_encode(const pcm_buf* in, const nofft_codec_opts* o,
                uint8_t** out, size_t* out_len)
{
    return acpcm_encode6_buf(out, out_len, in, opt_bits(o),
                             opt_block(o, 1024), opt_e(o), opt_rate(o));
}

Err acpcm_decode_wrap(const uint8_t* in, size_t in_len, pcm_buf* out)
{
    return acpcm_decode_buf(in, in_len, out);
}

Err acpcm_codec_info(const uint8_t* in, size_t in_len, nofft_codec_info* out)
{
    if (out == NULL)
        return ERR_BAD_ARGS;
    acpcm_info ai;
    Err e = acpcm_file_info_buf(in, in_len, &ai);
    if (e != ERR_OK)
        return e;
    out->sample_rate = ai.sample_rate;
    out->channels = ai.channels;
    out->frames = ai.frames;
    out->payload = ai.payload;
    out->bits = ai.bits;
    out->format = ai.format;
    return ERR_OK;
}

/* `id` is only used to tell the vtables apart in nofft_codec_builtin(); the
   codecs themselves are selected by name. */
const nofft_codec k_poly = { "poly", "polynomial fit (no_fft)", "no_fft",
                             NOFFT_CODEC_ABI, poly_encode, poly_decode, poly_info };
const nofft_codec k_nfa1 = { "nfa1", "DPCM + range coder, fixed predictor",
                             "nadc", NOFFT_CODEC_ABI, nfa1_encode, acpcm_decode_wrap,
                             acpcm_codec_info };
const nofft_codec k_nfa2 = { "nfa2", "NFA1 with a block-refit predictor",
                             "nadc", NOFFT_CODEC_ABI, nfa2_encode, acpcm_decode_wrap,
                             acpcm_codec_info };
const nofft_codec k_nfa3 = { "nfa3", "adaptive residual basket table",
                             "nadc", NOFFT_CODEC_ABI, nfa3_encode, acpcm_decode_wrap,
                             acpcm_codec_info };
const nofft_codec k_nfa4 = { "nfa4", "basket only, no predictor",
                             "nadc", NOFFT_CODEC_ABI, nfa4_encode, acpcm_decode_wrap,
                             acpcm_codec_info };
const nofft_codec k_nfa5 = { "nfa5", "multiplicative log-domain predictor (lossy)",
                             "nadc", NOFFT_CODEC_ABI, nfa5_encode, acpcm_decode_wrap,
                             acpcm_codec_info };
const nofft_codec k_nfa6 = { "nfa6", "NFA5 with a per-block log step (lossy)",
                             "nadc", NOFFT_CODEC_ABI, nfa6_encode, acpcm_decode_wrap,
                             acpcm_codec_info };

const nofft_codec* const k_builtins[] = {
    &k_poly, &k_nfa1, &k_nfa2, &k_nfa3, &k_nfa4, &k_nfa5, &k_nfa6
};

} /* namespace */

extern "C" {

const nofft_codec* nofft_codec_poly(void) { return &k_poly; }
const nofft_codec* nofft_codec_nfa1(void) { return &k_nfa1; }
const nofft_codec* nofft_codec_nfa2(void) { return &k_nfa2; }
const nofft_codec* nofft_codec_nfa3(void) { return &k_nfa3; }
const nofft_codec* nofft_codec_nfa4(void) { return &k_nfa4; }
const nofft_codec* nofft_codec_nfa5(void) { return &k_nfa5; }
const nofft_codec* nofft_codec_nfa6(void) { return &k_nfa6; }

size_t nofft_codec_builtin_count(void)
{
    return sizeof(k_builtins) / sizeof(k_builtins[0]);
}

const nofft_codec* nofft_codec_builtin_at(size_t index)
{
    if (index >= nofft_codec_builtin_count())
        return NULL;
    return k_builtins[index];
}

const nofft_codec* nofft_codec_builtin(const char* name)
{
    if (name == NULL)
        return NULL;
    for (size_t i = 0; i < nofft_codec_builtin_count(); i++)
        if (std::strcmp(k_builtins[i]->name, name) == 0)
            return k_builtins[i];
    return NULL;
}

} /* extern "C" */
