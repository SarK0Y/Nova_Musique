#ifndef NOFFT_CODEC_H
#define NOFFT_CODEC_H

/* nofft codec module ABI.
 *
 * A codec is a small, self describing object: a name, a description, a file
 * extension and a few function pointers.  The core library keeps a registry of
 * them, and every codec ships two ways:
 *
 *   - compiled into the core library, reachable with nofft_codec_builtin() and
 *     registered on demand, so an application that links the library has every
 *     codec with no runtime loading at all;
 *
 *   - as a separate loadable module (nofft_codec_<name>.so on Linux, .dll on
 *     Windows) that exports nofft_codec_plugin and registers the same codec
 *     when a host loads it.  A host that wants to discover codecs at run time
 *     ships the modules next to its binary and calls nofft_codec_autoload();
 *     one that wants everything static ignores the modules entirely.
 *
 * A codec works on interleaved float samples in memory (the pcm_buf the rest of
 * the library already uses), never on files.  The path in / path out helpers
 * below and in acpcm.h are thin wrappers, so the same codec drops into a CLI, a
 * media player or a service without a scratch file.
 *
 * The registry stores pointers, so a loaded module has to stay resident for the
 * life of the process; the loader keeps every handle it opens.
 */

#include <stddef.h>
#include <stdint.h>

#include "nofft.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Bumped whenever the layout of nofft_codec or nofft_codec_opts changes.  The
   loader refuses a module built against a different value rather than calling
   through a mismatched vtable. */
#define NOFFT_CODEC_ABI 1u

/* Which fields of nofft_codec_opts the caller actually set.  A codec falls back
   to its own default for any field left clear, so an application only names the
   knobs it cares about. */
enum {
    NOFFT_OPT_BITS      = 1u << 0,   /* acpcm quantiser width */
    NOFFT_OPT_DEGREE    = 1u << 1,   /* poly polynomial degree */
    NOFFT_OPT_COEF      = 1u << 2,   /* poly nofft_coeff_format */
    NOFFT_OPT_FIT       = 1u << 3,   /* poly nofft_fit */
    NOFFT_OPT_BLOCK     = 1u << 4,   /* acpcm NFA2/NFA6 block length */
    NOFFT_OPT_POLICY    = 1u << 5,   /* acpcm NFA2 acpcm_p2_policy */
    NOFFT_OPT_STEP      = 1u << 6,   /* NFA4 width in percent */
    NOFFT_OPT_BASKETS   = 1u << 7,   /* NFA4 basket count, beats STEP */
    NOFFT_OPT_FRAME_LEN = 1u << 8,   /* poly block length in frames */
    NOFFT_OPT_E         = 1u << 9,   /* NFA5/NFA6 ratio base */
    NOFFT_OPT_ERR       = 1u << 10,  /* NFA5 log-domain step */
    NOFFT_OPT_RATE      = 1u << 11,  /* NFA5 auto / NFA6 target bits per sample */
    NOFFT_OPT_AUTO      = 1u << 12   /* NFA5: auto-dial err to RATE */
};

/* acpcm NFA2 predictor policy, mirrored here so a host only needs codec.h.
   The values match acpcm_p2_policy. */
enum {
    NOFFT_POLICY_NEVER  = 0,
    NOFFT_POLICY_ALWAYS = 1,
    NOFFT_POLICY_DRIFT  = 2
};

typedef struct {
    uint32_t flags;
    uint16_t bits;
    uint16_t degree;
    uint16_t coef_format;   /* a nofft_coeff_format */
    uint16_t fit;           /* a nofft_fit */
    uint32_t block_len;
    uint32_t policy;        /* an acpcm_p2_policy */
    uint32_t step;
    uint32_t baskets;
    uint32_t frame_len;
    double   e;
    double   err;
    double   rate;
} nofft_codec_opts;

/* Fills `o` with the same defaults the command line tool uses; the flags stay
   clear so a codec still applies its own defaults unless the caller sets one. */
void nofft_codec_opts_default(nofft_codec_opts* o);

/* What a container says about itself, independent of codec family.  `payload`
   is the compressed byte count where it is well defined, otherwise the whole
   container size. */
typedef struct {
    uint32_t sample_rate;
    uint32_t channels;
    uint64_t frames;     /* frames per channel */
    uint64_t payload;    /* compressed bytes */
    uint16_t bits;
    uint8_t  format;     /* codec family specific id */
} nofft_codec_info;

typedef struct {
    const char* name;         /* selector, e.g. "nfa3" */
    const char* description;  /* one line, for `codec list` */
    const char* extension;    /* file extension without the dot, e.g. "nadc" */
    uint32_t    abi;          /* must equal NOFFT_CODEC_ABI */

    /* Interleaved float samples -> a malloc'd container, released by the caller
       with nofft_codec_free.  `opts` may be NULL for defaults. */
    Err (*encode)(const pcm_buf* in, const nofft_codec_opts* opts,
                  uint8_t** out, size_t* out_len);

    /* A container in memory -> interleaved float samples. */
    Err (*decode)(const uint8_t* in, size_t in_len, pcm_buf* out);

    /* Optional header-only query.  May be NULL. */
    Err (*info)(const uint8_t* in, size_t in_len, nofft_codec_info* out);
} nofft_codec;

/* Every module exports exactly this symbol; it hands back its codec. */
typedef int (*nofft_codec_plugin_fn)(const nofft_codec** out);

/* ---- builtin codecs, compiled into the core library ---- */
const nofft_codec* nofft_codec_poly(void);
const nofft_codec* nofft_codec_nfa1(void);
const nofft_codec* nofft_codec_nfa2(void);
const nofft_codec* nofft_codec_nfa3(void);
const nofft_codec* nofft_codec_nfa4(void);
const nofft_codec* nofft_codec_nfa5(void);
const nofft_codec* nofft_codec_nfa6(void);

size_t             nofft_codec_builtin_count(void);
const nofft_codec* nofft_codec_builtin_at(size_t index);
const nofft_codec* nofft_codec_builtin(const char* name);

/* ---- registry ---- */
/* 0 on success; non-zero when the name is already taken or the codec is
   malformed.  The registry stores the pointer, not a copy of the codec. */
int                nofft_codec_register(const nofft_codec* codec);
size_t             nofft_codec_count(void);
const nofft_codec* nofft_codec_at(size_t index);
/* Registry first, then the builtins, so a host can use a codec without ever
   registering or loading anything. */
const nofft_codec* nofft_codec_find(const char* name);

/* Releases a buffer returned by a codec's encode. */
void               nofft_codec_free(void* p);

/* ---- dynamic loading ---- */
/* Loads one module and registers the codec it exports.  0 on success. */
int  nofft_codec_load(const char* path);
/* Loads every module in `dir`; returns how many registered. */
int  nofft_codec_load_dir(const char* dir);
/* Search path for nofft_codec_autoload(): directories separated by ':' on
   POSIX and ';' on Windows.  `path` is copied. */
void nofft_codec_set_search_path(const char* path);
/* Registers every builtin, then loads every module on the search path (or, when
   none was set, the NOFFT_MODULE_PATH environment variable).  Returns the
   number of codecs now in the registry. */
int  nofft_codec_autoload(void);

/* ---- path in / path out convenience ---- */
Err nofft_codec_encode_file(const nofft_codec* codec, const pcm_buf* in,
                            const nofft_codec_opts* opts, const char* out_path);
Err nofft_codec_decode_file(const nofft_codec* codec, const char* in_path,
                            pcm_buf* out);

/* Whole-file helpers, exposed because a host often wants the bytes before it
   has chosen a codec.  Release a read buffer with nofft_codec_free. */
Err nofft_codec_read_file(const char* path, uint8_t** out, size_t* out_len);
Err nofft_codec_write_file(const char* path, const uint8_t* data, size_t len);

#ifdef __cplusplus
}
#endif

#endif
