#ifndef ACPCM_H
#define ACPCM_H

/* acpcm - sample-domain DPCM coder with an adaptive binary range coder.
 *
 * A completely different scheme from the variable-degree polynomial codec in
 * nofft.cpp: this one keeps no polynomial at all.  Each frame of N samples is
 * coded as a first difference against the previous reconstructed sample, the
 * difference is quantised to a uniform step, and the resulting symbols go
 * through an LZMA-style carryless range coder.
 *
 * Container layout, all integers little endian:
 *
 *   0   char     magic[4]   "NFA1"
 *   4   uint32   frames     frames per channel
 *   8   uint32   bits       quantiser width, 2..24
 *   12  uint32   channels
 *   16  uint32   sample_rate
 *   20  uint32   len[c]     byte length of each channel chunk
 *   ..  payload: the chunks, back to back
 *
 * Each chunk is range coded and starts with its own range-coded header, so a
 * chunk is not byte aligned and cannot be found by seeking: the length table
 * above is what makes random access possible.  Chunk header, in order:
 *
 *   peak_q  u16   channel peak, x /= peak -> x /= 32768
 *   mean_q  i16   channel mean, restored on decode
 *   a1_q    i16   predictor gain, a1 = a1_q / 16384
 *   step_q  u16   quantiser step in normalised units, step = step_q / 32768
 *   bits    u8    must match the container's bits
 *
 * Every one of those header fields is quantised before it is written, and the
 * encoder then codes the already-quantised value.  Rounding on the way out and
 * again on the way back in desynchronises the two sides by one LSB, and because
 * DPCM is recursive that desync persists to the last sample.
 *
 * NFA2 (magic "NFA2") is the same codec with a forward adaptive predictor.
 * The fixed header gains uint32 block_len at offset 20 (24 bytes total) and
 * the length table starts there instead of at 20.  Inside every chunk, before
 * the sample at i = k * block_len, the encoder sends one adaptive binary flag
 * and, when the flag is set, a new 16 bit a1_q that replaces the predictor
 * gain for the rest of the chunk.  The encoder decides the flags (policy:
 * never / always / drift); the decoder only obeys.  NFA1 files carry
 * block_len = 0 and no flags, so both formats share one decoder.
 *
 * NFA3 (magic "NFA3") is the basket codec: the quality is still one global a1
 * per chunk, but every frame is split into a coarse symbol i0_b — the basket,
 * the magnitude class of the residual, 0 for a zero residual — and a fine
 * symbol i1_r, the remaining mantissa bits.  i0_b is arithmetic coded from a
 * running int32 histogram of basket counts that both sides update on every
 * sample.  When a residual saturates the quantiser range (|q| = lim, a
 * transient the predictor cannot reach) the encoder escapes: it sends the
 * full 32 bit raw sample instead of the huge symbol, and the decoder resumes
 * the filter from it.  The fixed header is the NFA1 20 byte layout; there is
 * no block_len, no refit flag and no length-table offset change.
 *
 * NFA4 (magic "NFA4") is basket-only: no predictor and no residual, one symbol
 * per sample.  The header carries the shifted-grid range peak_q, the channel
 * minimum base_q, and the basket count nb; every sample maps to u = xq - base_q
 * and is floored onto nb equal baskets of peak_q, with factor = nb - 1:
 * i_b = floor(u * factor / peak_q).  The basket histogram is adaptive: it starts
 * at zero on both sides and is updated after every sample, so no table is
 * transmitted.  The decoder rebuilds the basket midpoint and returns
 * (base_q + mid) / 32768.  A flat channel has peak_q == 0 and decodes exactly.
 * The fixed header is the NFA1 20 byte layout (format 3, block_len 0).
 *
 * NFA5 (magic "NFA5") is the multiplicative, log-domain predictor.  Instead of
 * coding a difference against the previous sample it codes the *ratio*:
 * u[n] = u[n-1] * e^(K[n]*err), where u = xq + ACPCM5_OFFSET is shifted onto a
 * strictly positive grid so the ratio is always defined.  K is the only symbol
 * per sample; e (base) and err (log-domain quantum of the ratio) travel in the
 * chunk header as IEEE-754 doubles, so the decoder rebuilds the same step
 * table.  The table F[K] = round(e^(K*err) * 2^16) is built once on each side
 * from the decoded doubles, so the sample loop is integer only.  The coding is
 * lossy by construction: the geometric grid cannot hit every integer, so a
 * round trip is one quantiser step away, not bit exact.  The fixed header is
 * the NFA1 20 byte layout (format 4, block_len 0).
 *
 * NFA6 (magic "NFA6") is NFA5 with NFA2's blocked container.  The fixed header
 * again gains uint32 block_len at offset 20 (24 bytes total).  Inside every
 * chunk the encoder re-fits the log-domain quantum err at each block boundary
 * as a per-block rate control: it bisects err until the block's estimated K
 * cost fits a per-block slice of the target bits/sample, then range-codes one
 * 8 bit log-spaced code before the first K of the block.  The decoder reads
 * the code and rebuilds the same step table; the only transmitted state is one
 * byte per block and the encoder's policy is never seen or trusted.  Quiet
 * blocks keep a fine step almost free (their K are mostly 0), loud or fast
 * blocks coarsen to stay in budget, so the target rate is met block by block
 * instead of only on average - which is exactly what a fixed err cannot do
 * (a coarse err that is right for the loudest passage is many sample units
 * wide near silence, and a fine one is extortionate on a fast sweep).  e stays
 * per chunk; err is per block and is never an argument.  Lossy like NFA5.
 */

#include <stdint.h>
#include <stddef.h>

#include "nofft.h"

#define ACPCM_MAGIC         "NFA1"
#define ACPCM_MAGIC2        "NFA2"
#define ACPCM_MAGIC3        "NFA3"
#define ACPCM_MAGIC4        "NFA4"
#define ACPCM_MAGIC5        "NFA5"
#define ACPCM_MAGIC6        "NFA6"
#define ACPCM_FIXED_HEADER  20
#define ACPCM_FIXED_HEADER2 24
#define ACPCM_BITS_MIN      2
#define ACPCM_BITS_MAX      24
#define ACPCM_MAX_BASKETS   25   /* classes 0..bits-1 plus the escape class */
/* NFA4: basket grid of `baskets` equal buckets over the shifted channel range.
   The count, not a step percent, is stored in the header, so the grid is not
   capped at the 101 rows an integer-percent step allows.  256 is a safety
   bound (the alphabet is scanned once per sample, so this bounds decode cost). */
#define ACPCM4_MAX_BUCKETS  256
#define ACPCM4_BASKETS_MIN  1
#define ACPCM4_STEP_MIN     1
#define ACPCM4_STEP_MAX     65535

/* NFA5: multiplicative (log-domain) predictor.  u = xq + offset is the shifted,
   strictly positive grid the ratio lives on; the offset is one past 32768 so no
   sample ever lands on u = 0, where the recurrence could not leave.  `err` is
   the log-domain quantum of the per-step ratio e^err, so the grid is geometric
   and the codec is lossy.  ACPCM5_MAX_STEPS caps the |K| table; the smallest
   accepted err (with e > 1) therefore still has to span the whole grid. */
#define ACPCM5_OFFSET      32769
#define ACPCM5_MAX_STEPS   32768
#define ACPCM5_E_DEFAULT   2.0
#define ACPCM5_ERR_DEFAULT 0.01
#define ACPCM5_ERR_MIN     5e-4
#define ACPCM5_ERR_MAX     1.0

/* NFA6: err is auto-selected per block (never an argument); the only dial is
   the target bits/sample, which each block's encoder-side fit tries to meet. */
#define ACPCM6_RATE_DEFAULT 2.6

/* NFA2: when does the encoder refit a1 at a block boundary?  NEVER keeps the
   stream bit identical to NFA1 (flags all zero); the refit itself is decided
   encoder side, the decoder only reads what was sent. */
typedef enum {
    ACP2_NEVER  = 0,
    ACP2_ALWAYS = 1,
    ACP2_DRIFT  = 2
} acpcm_p2_policy;

typedef struct {
    uint16_t bits;       /* quantiser width actually used */
    uint32_t channels;
    uint32_t sample_rate;
    uint32_t block_len;  /* NFA2/NFA6 block length in frames, 0 otherwise */
    uint8_t  format;     /* 0: NFA1, 1: NFA2, 2: NFA3, 3: NFA4, 4: NFA5, 5: NFA6 */
    uint64_t frames;     /* frames per channel */
    uint64_t payload;    /* bytes of range coded payload */
    uint64_t total;      /* whole file size */
} acpcm_info;

/* The replacement codes an NFA1 (or NFA2) chunk actually transmits: the
   quantised residual symbol q of every frame after the first, per channel.
   The first sample of a chunk is a raw 16 bit seed, not a symbol, so it has no
   code and is absent from the sequence.  Filled by acpcm_extract_codes, which
   decodes the range coded symbols only and reconstructs no audio. */
typedef struct {
    uint32_t channels;
    uint64_t frames;   /* frames per channel, from the container */
    uint64_t codes;    /* codes per channel: frames - 1 (0 when frames <= 1) */
    int32_t* q;        /* channels * codes, channel major; NULL when codes == 0 */
} acpcm_codes;

/* p->samples is interleaved.  `bits` outside ACPCM_BITS_MAX..ACPCM_BITS_MAX is
   rejected with ERR_RANGE. */
Err acpcm_encode(const char* path, const pcm_buf* p, uint16_t bits);

/* NFA2 with refits every block_len frames; block_len 0 and a policy outside
   NEVER..DRIFT are rejected with ERR_RANGE. */
Err acpcm_encode2(const char* path, const pcm_buf* p, uint16_t bits,
                  uint32_t block_len, acpcm_p2_policy policy);

/* NFA3, the basket codec: one global predictor, coarse basket symbol +
   fine mantissa symbol, adaptive int32 histogram counts. */
Err acpcm_encode3(const char* path, const pcm_buf* p, uint16_t bits);

/* NFA4, the basket-only codec: the channel is shifted so its minimum is at
   zero (base_q), and every sample carries one basket index
   i_b = floor((xq - base_q) * (nb - 1) / peak_q) into `nb` equal buckets of
   the shifted range.  The decoder rebuilds the bucket midpoint and adds base_q
   back; there is no predictor and no residual.  The basket histogram is
   adaptive: starts at zero on both sides, updates after every sample, and is
   not transmitted.  `bits` is stored for the header but does not affect the
   grid.  This entry point takes a bucket width in percent and maps it to
   nb = 100/step + 1 (so step 1 gives 101 baskets); it is a convenience wrapper
   over acpcm_encode4_baskets.  `step` outside ACPCM4_STEP_MIN..ACPCM4_STEP_MAX
   is rejected with ERR_RANGE. */
Err acpcm_encode4(const char* path, const pcm_buf* p, uint16_t bits,
                  uint32_t step);

/* NFA4 with the basket count given directly, 1..ACPCM4_MAX_BUCKETS.  This is
   the only way to ask for more than the 101 buckets the integer-percent step
   grid can express (e.g. 170).  A count outside the range is rejected with
   ERR_RANGE. */
Err acpcm_encode4_baskets(const char* path, const pcm_buf* p, uint16_t bits,
                          uint32_t baskets);

/* NFA5, the multiplicative (log-domain) predictor: the only symbol per sample
   is K, the integer multiple of `err` in S[n] = S[n-1] * e^(K*err) on the
   shifted, strictly positive grid u = xq + ACPCM5_OFFSET.  `e` (base) and `err`
   (log-domain quantum) are stored as doubles in every chunk, so the decoder
   rebuilds the same fixed-point step table.  This codec is lossy: the geometric
   grid cannot reproduce every integer sample.  `e` must be > 1 and finite; `err`
   must be in ACPCM5_ERR_MIN..ACPCM5_ERR_MAX; anything else is ERR_RANGE.  `bits`
   is stored in the header but does not affect the grid. */
Err acpcm_encode5(const char* path, const pcm_buf* p, uint16_t bits,
                  double e, double err);

/* NFA6, NFA5 with a per-block re-fit of the log-domain quantum (see the
   container note above): the header carries block_len, and every block's err is
   chosen by an encoder-side rate control - bisected so the block's estimated K
   cost fits a per-block slice of `rate` bits/sample - then range-coded as one
   8 bit log-spaced code.  Quiet blocks stay fine almost free, loud or fast
   blocks coarsen to stay in budget, and the target rate is met block by block.
   `rate` must be finite and > 0; `e` must be > 1 and finite; `block_len` must
   be >= 1.  Lossy like NFA5. */
Err acpcm_encode6(const char* path, const pcm_buf* p, uint16_t bits,
                  uint32_t block_len, double e, double rate);

/* NFA5 with an auto-dialed global err: one err per chunk, bisected so the
   chunk's estimated K cost fits `rate` bits/sample.  This is the mis-dial
   cure - a quiet file dials itself fine (and stays cheap because its K are
   mostly 0), a loud or fast one coarsens to hold the rate, and budget
   migrates from easy to hard material the way hand tuning would - without
   NFA6's per-block equal budgets, which overpay quiet blocks and starve hard
   ones on mixed material.  The file format is plain NFA5; only the choice of
   err is automatic.  `rate` must be finite and > 0. */
Err acpcm_encode5_auto(const char* path, const pcm_buf* p, uint16_t bits,
                       double e, double rate);

Err acpcm_decode(const char* path, pcm_buf* out);

/* ---- in-memory buffer API ----
 *
 * The same containers the path based calls read and write, but handed back in a
 * malloc'd block and decoded straight from memory.  A host application, or a
 * codec module, uses these; the path calls above are thin file wrappers over
 * them and produce byte identical output.  Release an encoded buffer with
 * acpcm_buf_free.  The encode variants mirror acpcm_encode / 2 / 3 / 4 /
 * 4_baskets / 5 / 6 / 5_auto and apply the same argument validation. */
Err acpcm_encode_buf(uint8_t** out, size_t* out_len, const pcm_buf* p, uint16_t bits);
Err acpcm_encode2_buf(uint8_t** out, size_t* out_len, const pcm_buf* p, uint16_t bits,
                      uint32_t block_len, acpcm_p2_policy policy);
Err acpcm_encode3_buf(uint8_t** out, size_t* out_len, const pcm_buf* p, uint16_t bits);
Err acpcm_encode4_buf(uint8_t** out, size_t* out_len, const pcm_buf* p, uint16_t bits,
                      uint32_t step);
Err acpcm_encode4_baskets_buf(uint8_t** out, size_t* out_len, const pcm_buf* p,
                              uint16_t bits, uint32_t baskets);
Err acpcm_encode5_buf(uint8_t** out, size_t* out_len, const pcm_buf* p, uint16_t bits,
                      double e, double err);
Err acpcm_encode6_buf(uint8_t** out, size_t* out_len, const pcm_buf* p, uint16_t bits,
                      uint32_t block_len, double e, double rate);
Err acpcm_encode5_auto_buf(uint8_t** out, size_t* out_len, const pcm_buf* p,
                           uint16_t bits, double e, double rate);
Err acpcm_decode_buf(const uint8_t* data, size_t len, pcm_buf* out);
Err acpcm_file_info_buf(const uint8_t* data, size_t len, acpcm_info* out);
void acpcm_buf_free(void* p);

/* Header only, no decoding. */
Err acpcm_file_info(const char* path, acpcm_info* out);

/* Decodes the residual symbols of an NFA1 (or NFA2) stream into `out` without
   rebuilding the samples; the refit flags NFA2 codes are consumed so the two
   sides stay in step.  NFA3/NFA4/NFA5/NFA6 carry no single residual symbol
   and are rejected with ERR_UNSUPPORTED_FMT.  Release the result with
   acpcm_codes_free. */
Err acpcm_extract_codes(const char* path, acpcm_codes* out);
void acpcm_codes_free(acpcm_codes* c);

/* ---- streaming decode ----
 *
 * Every channel is its own range-coded stream, so a stereo file cannot be
 * decoded one channel at a time and stitched together afterwards without
 * buffering the whole thing.  This advances all channels in lockstep and
 * interleaves them as it goes, which is what makes bounded-memory playback
 * possible.
 */
struct AcpcmDecoder;

/* Reads the container and both channels' headers; no audio is decoded yet. */
Err acpcm_dec_open(const char* path, AcpcmDecoder** out);
void acpcm_dec_close(AcpcmDecoder* d);
const acpcm_info* acpcm_dec_info(const AcpcmDecoder* d);

/* Decodes up to max_frames interleaved frames into `out`, which must have room
   for max_frames * channels floats.  *got is the number of frames produced; a
   short count means end of stream.  Memory use does not depend on file length. */
Err acpcm_dec_read(AcpcmDecoder* d, float* out, size_t max_frames, size_t* got);

/* ---- playback ----
 *
 * Decodes straight to ALSA, so a long file does not have to be held in memory.
 * `device` may be NULL for the default.  Built without ALSA the call reports
 * ERR_UNSUPPORTED_FMT and the codec still encodes and decodes.
 */
Err acpcm_play(const char* path, const char* device);

/* ---- test signal ----
 *
 * A deterministic stereo/mono tone-and-chirp WAV, so the codec can be checked
 * without shipping an input file.
 */
Err acpcm_gen(const char* path, double seconds, uint32_t channels, uint32_t sample_rate);

#endif