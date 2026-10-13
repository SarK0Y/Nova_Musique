#include "acpcm.h"
#include "nofft.h"

#include <cmath>
#include <cstdio>
#include <cstring>
#include <cstdlib>
#include <vector>
#include <new>
#include <algorithm>


/* ---- LZMA style carryless range coder -------------------------------
 *
 * `low` must be 64 bit.  A carry out of the bound addition lives above bit 31
 * and is consumed by the byte output; truncating low to 32 bits loses it and
 * the stream decodes to garbage.  Adaptation is a 5 bit shift of the distance
 * to 2048, one shared model per call for fixed width fields.
 */
namespace {

const uint16_t RC_PROB_INIT = 2048 >> 1;
const uint32_t RC_TOP       = 1u << 24;

void rc_shift_low(std::vector<uint8_t>& out, uint64_t& low,
                  uint32_t& range, uint8_t& cache, int64_t& cache_size)
{
    if ((uint32_t)(low >> 32) != 0 || (uint32_t)low < 0xFF000000u) {
        uint8_t carry = (uint8_t)(low >> 32);
        uint8_t temp = cache;
        do {
            out.push_back((uint8_t)(temp + carry));
            temp = 0xFF;
        } while (--cache_size != 0);
        cache = (uint8_t)((uint32_t)low >> 24);
    }
    cache_size++;
    low = (uint32_t)low << 8;
    range <<= 8;
}

void rc_enc_bit(std::vector<uint8_t>& out, uint64_t& low, uint32_t& range,
                uint8_t& cache, int64_t& cache_size, uint16_t* prob, int bit)
{
    uint32_t bound = (range >> 11) * (uint32_t)(*prob);
    if (bit) {
        low += bound;
        range -= bound;
        *prob = (uint16_t)(*prob - (*prob >> 5));
    } else {
        range = bound;
        *prob = (uint16_t)(*prob + ((2048u - (uint32_t)(*prob)) >> 5));
    }
    while (range < RC_TOP)
        rc_shift_low(out, low, range, cache, cache_size);
}

/* Five shift-lows, not a run of dummy bits: a dummy-bit flush can leave the
   final interval undecodable, which shows up as the last couple of samples of
   every file coming out wrong. */
void rc_flush(std::vector<uint8_t>& out, uint64_t& low, uint32_t& range,
              uint8_t& cache, int64_t& cache_size)
{
    for (int i = 0; i < 5; i++)
        rc_shift_low(out, low, range, cache, cache_size);
}

void rc_enc_fixed(std::vector<uint8_t>& out, uint64_t& low, uint32_t& range,
                  uint8_t& cache, int64_t& cache_size, uint32_t v, int n)
{
    uint16_t p = RC_PROB_INIT;
    for (int i = n - 1; i >= 0; i--)
        rc_enc_bit(out, low, range, cache, cache_size, &p, (int)((v >> i) & 1u));
}

/* Multi-symbol arithmetic coding from a histogram: the alphabet is the M
   basket classes, symbol sym gets probability (cnt[sym]+1)/(total+M), so an
   unobserved class still has a nonzero interval and both sides can decode an
   empty model.  The encoder and the decoder must round these divisions the
   same way; they do: both use the exact same floor products on the same
   (cumulative, total) pair. */
void rc_enc_multi(std::vector<uint8_t>& out, uint64_t& low, uint32_t& range,
                  uint8_t& cache, int64_t& cache_size,
                  const int32_t* cnt, int M, int64_t tot, int sym)
{
    int64_t cum = 0;
    for (int j = 0; j < sym; j++)
        cum += (int64_t)cnt[j] + 1;
    int64_t tm = tot + M;
    uint64_t l = ((uint64_t)range * (uint64_t)cum) / (uint64_t)tm;
    uint32_t h = (uint32_t)(((uint64_t)range * (uint64_t)(cum + (int64_t)cnt[sym] + 1)) /
                            (uint64_t)tm);
    low += l;
    range = h - (uint32_t)l;
    if (range == 0)
        range = 1;
    while (range < RC_TOP)
        rc_shift_low(out, low, range, cache, cache_size);
}

/* Basket frequency index for NFA4: a Fenwick (binary indexed) tree over the
   same frequencies rc_enc_multi and DecCtx::multi use, f[j] = cnt[j] + 1.  It
   makes the encoder prefix sum and the decoder's search for the symbol that
   holds `code` O(log M) instead of an O(M) scan, while computing the exact
   same integer products, so the bitstream is unchanged.  `tot` is the codec's
   sample counter (not the sum of f): the model uses tm = tot + M, matching the
   linear version including its halving rule. */
struct BasketTree {
    int32_t cnt[ACPCM4_MAX_BUCKETS];
    int64_t tree[ACPCM4_MAX_BUCKETS + 1];
    int M;
    int64_t tot;

    void init(int m)
    {
        M = m;
        for (int i = 0; i <= M; i++)
            tree[i] = 0;
        for (int i = 0; i < M; i++) {
            cnt[i] = 0;
            add(i, 1);
        }
        tot = 0;
    }

    void add(int i, int64_t v)
    {
        for (i++; i <= M; i += i & -i)
            tree[i] += v;
    }

    int64_t pref(int j) const   /* sum_{k < j} f[k] */
    {
        int64_t s = 0;
        for (; j > 0; j -= j & -j)
            s += tree[j];
        return s;
    }

    int find(int64_t x) const   /* largest j with pref(j) <= x */
    {
        int pos = 0;
        int bit = 1;
        while (bit <= M)
            bit <<= 1;
        for (bit >>= 1; bit; bit >>= 1) {
            int nx = pos + bit;
            if (nx <= M && tree[nx] <= x) {
                pos = nx;
                x -= tree[nx];
            }
        }
        return pos < M ? pos : M - 1;
    }

    void bump(int sym)
    {
        cnt[sym]++;
        tot++;
        add(sym, 1);
    }

    void rescale()
    {
        for (int i = 0; i <= M; i++)
            tree[i] = 0;
        for (int i = 0; i < M; i++) {
            cnt[i] >>= 1;
            add(i, (int64_t)cnt[i] + 1);
        }
        tot >>= 1;
    }
};

struct EncCtx {
    std::vector<uint8_t> out;
    uint64_t low;
    uint32_t range;
    uint8_t cache;
    int64_t cache_size;

    EncCtx() : low(0), range(0xFFFFFFFFu), cache(0), cache_size(1) {}

    void bit(uint16_t* prob, int b)
    {
        rc_enc_bit(out, low, range, cache, cache_size, prob, b);
    }
    void fixed(uint32_t v, int n)
    {
        rc_enc_fixed(out, low, range, cache, cache_size, v, n);
    }
    void multi(const int32_t* cnt, int M, int64_t tot, int sym)
    {
        rc_enc_multi(out, low, range, cache, cache_size, cnt, M, tot, sym);
    }
    /* Same as multi(), with the cumulative frequency `cum` and the symbol
       frequency `f` already known so a Fenwick tree can supply them in O(log M)
       instead of the O(M) prefix scan. */
    void multi_cum(int64_t cum, int64_t f, int64_t tm)
    {
        uint64_t l = ((uint64_t)range * (uint64_t)cum) / (uint64_t)tm;
        uint32_t h = (uint32_t)(((uint64_t)range * (uint64_t)(cum + f)) /
                                (uint64_t)tm);
        low += l;
        range = h - (uint32_t)l;
        if (range == 0)
            range = 1;
        while (range < RC_TOP)
            rc_shift_low(out, low, range, cache, cache_size);
    }
    void done() { rc_flush(out, low, range, cache, cache_size); }
};

/* ---- decoder ---- */

struct DecCtx {
    const uint8_t* d;
    size_t n;
    size_t pos;
    uint32_t range;
    uint32_t code;

    void norm()
    {
        while (range < RC_TOP) {
            range <<= 8;
            code = (code << 8) | (pos < n ? (uint32_t)d[pos++] : 0u);
        }
    }

    int bit(uint16_t* prob)
    {
        uint32_t bound = (range >> 11) * (uint32_t)(*prob);
        int b;
        if (code < bound) {
            b = 0;
            range = bound;
            *prob = (uint16_t)(*prob + ((2048u - (uint32_t)(*prob)) >> 5));
        } else {
            b = 1;
            code -= bound;
            range -= bound;
            *prob = (uint16_t)(*prob - (*prob >> 5));
        }
        norm();
        return b;
    }

    uint32_t fixed(int n)
    {
        uint32_t v = 0;
        uint16_t p = RC_PROB_INIT;
        for (int i = 0; i < n; i++)
            v = (v << 1) | (uint32_t)bit(&p);
        return v;
    }

    int multi(const int32_t* cnt, int M, int64_t tot)
    {
        int64_t tm = tot + M;
        int64_t cum = 0;
        int j;
        for (j = 0; j < M; j++) {
            int64_t cum_next = cum + (int64_t)cnt[j] + 1;
            uint32_t hi = (uint32_t)(((uint64_t)range * (uint64_t)cum_next) /
                                     (uint64_t)tm);
            if (code < hi)
                break;
            cum = cum_next;
        }
        if (j >= M)
            j = M - 1;
        int64_t cum_hi = cum + (int64_t)cnt[j] + 1;
        uint32_t lo = (uint32_t)(((uint64_t)range * (uint64_t)cum) /
                                 (uint64_t)tm);
        uint32_t hi = (uint32_t)(((uint64_t)range * (uint64_t)cum_hi) /
                                 (uint64_t)tm);
        code -= lo;
        range = hi - lo;
        if (range == 0)
            range = 1;
        norm();
        return j;
    }

    /* Decode one symbol against a Fenwick basket model.  The symbol is the
       largest index whose cumulative frequency pref(j) is <= x, where
       x = (tm * (code + 1) - 1) / range is exactly the largest cumulative
       boundary the linear scan in multi() would accept; the interval is then
       advanced with the same floor products. */
    int multi_tree(const BasketTree* t)
    {
        int64_t tm = t->tot + (int64_t)t->M;
        int64_t x = ((int64_t)tm * ((int64_t)code + 1) - 1) / (int64_t)range;
        int j = t->find(x);
        int64_t cum = t->pref(j);
        int64_t f = (int64_t)t->cnt[j] + 1;
        uint32_t lo = (uint32_t)(((uint64_t)range * (uint64_t)cum) /
                                 (uint64_t)tm);
        uint32_t hi = (uint32_t)(((uint64_t)range * (uint64_t)(cum + f)) /
                                 (uint64_t)tm);
        code -= lo;
        range = hi - lo;
        if (range == 0)
            range = 1;
        norm();
        return j;
    }
};

/* ---- symbol model ----
 *
 * zero flag, then a unary "how many bits is the magnitude" run, then that many
 * mantissa bits, then the sign.  A q of 0 is by far the most common value so it
 * gets the single adaptive flag.
 */
struct SymModel {
    uint16_t p_zero;
    uint16_t p_sign;
    uint16_t p_run[64];
    uint16_t p_type;
    uint16_t p_flag;  /* NFA2 refit flag, one bit per block boundary */
};

void sym_init(SymModel* m)
{
    m->p_zero = RC_PROB_INIT;
    m->p_sign = RC_PROB_INIT;
    m->p_type = RC_PROB_INIT;
    m->p_flag = RC_PROB_INIT;
    for (int i = 0; i < 64; i++)
        m->p_run[i] = RC_PROB_INIT;
}

void enc_sym(EncCtx* e, SymModel* m, int32_t q)
{
    e->bit(&m->p_type, 0);
    if (q == 0) {
        e->bit(&m->p_zero, 0);
        return;
    }
    e->bit(&m->p_zero, 1);
    uint32_t mag = (q < 0) ? (uint32_t)(-q) : (uint32_t)q;
    int k = 0;
    while ((mag >> k) != 0)
        k++;
    for (int j = 1; j < k; j++)
        e->bit(&m->p_run[j], 0);
    e->bit(&m->p_run[k], 1);
    for (int j = k - 2; j >= 0; j--)
        e->fixed((mag >> j) & 1u, 1);
    e->bit(&m->p_sign, q < 0 ? 1 : 0);
}

int32_t dec_sym(DecCtx* d, SymModel* m)
{
    if (d->bit(&m->p_type) != 0)
        return 0x7FFFFFFF; /* should not happen if used correctly */
    if (d->bit(&m->p_zero) == 0)
        return 0;
    int k = 1;
    while (d->bit(&m->p_run[k]) == 0)
        k++;
    uint32_t mag = 1;
    for (int j = k - 2; j >= 0; j--)
        mag = (mag << 1) | d->fixed(1);
    int32_t q = (int32_t)mag;
    return d->bit(&m->p_sign) ? -q : q;
}



/* ---- NFA3 basket model ----
 *
 * The basket is the coarse half of a two-stage symbol.  q == 0 is basket 0;
 * otherwise basket k is the number of bits of |q| (1..bits-1), so the baskets
 * grow geometrically wider and the common small residuals get the narrowest
 * classes.  The model is first order: the previous sample's basket selects the
 * row, so the histogram learns "given the last class, what class comes next" --
 * the residual class is strongly autocorrelated (a quiet run stays quiet, a
 * transient is followed by decay).  Both sides update the row on every sample.
 * A row is halved when its total grows past 2^20, which keeps every range-coder
 * product in 64 bits.
 */
struct BasketModel {
    int32_t cnt[ACPCM_MAX_BASKETS][ACPCM_MAX_BASKETS];
    int64_t tot[ACPCM_MAX_BASKETS];
};

void basket_init(BasketModel* b)
{
    for (int i = 0; i < ACPCM_MAX_BASKETS; i++) {
        for (int j = 0; j < ACPCM_MAX_BASKETS; j++)
            b->cnt[i][j] = 0;
        b->tot[i] = 0;
    }
}

void basket_bump(BasketModel* b, int ctx, int sym)
{
    b->cnt[ctx][sym]++;
    b->tot[ctx]++;
    if (b->tot[ctx] > (int64_t)(1 << 20)) {
        for (int i = 0; i < ACPCM_MAX_BASKETS; i++)
            b->cnt[ctx][i] >>= 1;
        b->tot[ctx] >>= 1;
    }
}

/* Basket for a clamped residual q.  `classes` is bits+1: 0..bits-1 are real
   magnitude classes and index `classes-1` is the escape -- a fully saturated
   q, a transient the predictor cannot reach, which is coded as a raw sample
   instead of a huge symbol. */
int basket_class(int32_t q, int32_t lim, int classes)
{
    if (q == 0)
        return 0;
    uint32_t mag = (q < 0) ? (uint32_t)(-q) : (uint32_t)q;
    if (mag >= (uint32_t)lim)
        return classes - 1;
    int k = 0;
    while (mag != 0) {
        k++;
        mag >>= 1;
    }
    return k;
}

/* ---- little endian helpers ----
 *
 * The polynomial format in nofft.cpp serialises with memcpy of native ints, so
 * its files are host endian.  These are explicit instead.
 */
void put_u32(uint8_t* d, uint32_t v)
{
    d[0] = (uint8_t)(v & 0xFF);
    d[1] = (uint8_t)((v >> 8) & 0xFF);
    d[2] = (uint8_t)((v >> 16) & 0xFF);
    d[3] = (uint8_t)((v >> 24) & 0xFF);
}

uint32_t get_u32(const uint8_t* d)
{
    return (uint32_t)d[0] | ((uint32_t)d[1] << 8) |
           ((uint32_t)d[2] << 16) | ((uint32_t)d[3] << 24);
}

/* Fixed point rounding, always half away from zero, matching llround. */
double quant_fixed(double v, double scale);

/* Scales, rounds, then clamps into the range the wire field can hold.  Clamping
   matters: a normalised sample at the channel peak rounds to exactly 32768,
   which wraps to -32768 as a signed 16 bit field and flips the sign of the
   first reconstructed sample. */
int32_t quant_clamp(double v, double scale, double lo, double hi)
{
    double s = quant_fixed(v, scale);
    if (s < lo)
        s = lo;
    if (s > hi)
        s = hi;
    return (int32_t)s;
}

double quant_fixed(double v, double scale)
{
    double s = v * scale;
    double r = (s >= 0.0) ? std::floor(s + 0.5) : std::ceil(s - 0.5);
    return r;
}

/* Predictor gain is held strictly inside the unit circle.  An open loop fit of
   this material wants a1 = 1.437, whose pole sits at 1.72 and whose closed
   loop saturates; clamping costs nothing here because the real fit is 0.969. */
const double A1_LIMIT = 0.999;

struct ChanPlan {
    uint16_t peak_q;
    int16_t mean_q;
    int16_t a1_q;
    uint16_t step_q;
    double peak;
    double mean;
    double a1;
    double step;
};

/* Header fields are 16 bit fixed point over a nominal [-1, 1] signal.  Input
   outside that range cannot be represented, and clamping it into range silently
   produces a stream that decodes to noise, so reject it instead. */
const double PEAK_LIMIT = 2.0;

/* One pass over a channel: peak, mean, least squares gain, residual peak. */
Err plan_channel(const float* s, size_t n, uint16_t bits, ChanPlan* pl)
{
    double peak = 0.0, mean = 0.0;
    for (size_t i = 0; i < n; i++) {
        double v = s[i];
        double a = (v < 0.0) ? -v : v;
        if (!(a == a) || a > PEAK_LIMIT)
            return ERR_BAD_INPUT; /* NaN, or too hot for the header fields */
        if (a > peak)
            peak = a;
        mean += v;
    }
    mean /= (double)n;
    if (peak < 1e-9)
        peak = 1.0; /* silence: avoid dividing by zero */

    double num = 0.0, den = 0.0;
    for (size_t i = 1; i < n; i++) {
        double p = (s[i - 1] - mean) / peak;
        double c = (s[i] - mean) / peak;
        num += p * c;
        den += p * p;
    }

    double a1 = (den > 1e-12) ? (num / den) : 0.0;
    if (a1 > A1_LIMIT)
        a1 = A1_LIMIT;
    if (a1 < -A1_LIMIT)
        a1 = -A1_LIMIT;

    /* residual peak sets the step so the largest residual lands just inside
       the quantiser range */
    double rpk = 0.0;
    for (size_t i = 1; i < n; i++) {
        double p = (s[i - 1] - mean) / peak;
        double c = (s[i] - mean) / peak;
        double r = c - a1 * p;
        double ar = (r < 0.0) ? -r : r;
        if (ar > rpk)
            rpk = ar;
    }

    /* Map the largest residual just inside the symbol range, then quantise that
       step into the 16 bit header field and widen it until the peak really
       does fit.

       Two things push it out of range.  The formula below puts the peak at
       (2^bits - 1) / 2, which rounds to lim + 1, one symbol too far.  And the
       header can only hold the step as 16 bit fixed point, so rounding may land
       on a *finer* step than the one rpk was measured against.

       Clamping is expensive here.  The filter is recursive, so one clipped
       sample leaves an error behind that decays only as fast as 1 / (1 - a1),
       around 30 samples at a1 = 0.97.  Widen the step by a fraction of a
       symbol instead: it costs a little precision and avoids the clip. */
    double lim = (double)(((uint32_t)1 << (bits - 1)) - 1u);
    double cap = (lim - 1.0 > 1.0) ? (lim - 1.0) : 1.0;
    double step = 2.0 * rpk / (double)(((uint32_t)1 << bits) - 1u);
    double step_q = quant_fixed(step, 32768.0);

    if (rpk > 0.0) {
        /* step must be at least rpk / cap, or the largest residual lands past
           the last symbol.  Round that bound *up*: rounding to nearest can
           land on a finer step than the bound allows, which is exactly the case
           that clips. */
        double need_q = quant_fixed(rpk / cap, 32768.0);
        if (need_q / 32768.0 < rpk / cap)
            need_q += 1.0;
        if (step_q < need_q)
            step_q = need_q;
    }
    if (step_q < 1.0)
        step_q = 1.0;
    if (step_q > 65535.0)
        step_q = 65535.0;

    /* Quantise once here, then write the quantised value.  Doing the rounding
       again on the decode side shifts the gain by one LSB and the recursive
       filter never resynchronises. */
    pl->peak_q = (uint16_t)quant_clamp(peak, 32768.0, 0.0, 65535.0);
    pl->mean_q = (int16_t)quant_clamp(mean, 32768.0, -32768.0, 32767.0);
    pl->a1_q = (int16_t)quant_clamp(a1, 16384.0, -32768.0, 32767.0);
    pl->step_q = (uint16_t)step_q;

    pl->peak = (double)pl->peak_q / 32768.0;
    pl->mean = (double)pl->mean_q / 32768.0;
    pl->a1 = (double)pl->a1_q / 16384.0;
    pl->step = (double)pl->step_q / 32768.0;
    if (pl->peak < 1e-9)
        pl->peak = 1.0;

    return ERR_OK;
}

/* ---- NFA2: basket meter and a1 refit ----
 *
 * A basket is the block's worth of residuals about to be coded.  When a1 is
 * wrong for the material in it, |q| climbs into the range that costs real
 * bits per sample (a unary run plus mantissa) even though it is nowhere near
 * the symbol limit, so the meter counts |q| >= lim / ACP2_BASKET_DEN and
 * fires the refit once that fraction of the block looks expensive. */
const int ACP2_BASKET_DEN = 8;
const int ACP2_DRIFT_PCT  = 1;

/* Least squares a1 over the source pairs (j-1, j), j in [j0, j1).  Quantised
   exactly like plan_channel, so encoder and decoder keep the same value; a
   flat window has nothing to learn and keeps the current gain. */
int16_t refit_a1_q(const float* s, size_t j0, size_t j1,
                   double mean, double peak, int16_t cur)
{
    double num = 0.0, den = 0.0;
    for (size_t j = j0; j < j1; j++) {
        double p = (s[j - 1] - mean) / peak;
        double c = (s[j] - mean) / peak;
        num += p * c;
        den += p * p;
    }
    if (den <= 1e-12)
        return cur;
    double a1 = num / den;
    if (a1 > A1_LIMIT)
        a1 = A1_LIMIT;
    if (a1 < -A1_LIMIT)
        a1 = -A1_LIMIT;
    return (int16_t)quant_clamp(a1, 16384.0, -32768.0, 32767.0);
}

Err encode_chunk(const float* s, size_t n, const ChanPlan& pl,
                 uint16_t bits, std::vector<uint8_t>* out,
                 uint32_t block_len, acpcm_p2_policy policy)
{
    int32_t lim = (int32_t)((1u << (bits - 1)) - 1u);
    SymModel m;
    sym_init(&m);

    EncCtx e;
    e.fixed(pl.peak_q, 16);
    e.fixed((uint32_t)(uint16_t)pl.mean_q, 16);
    e.fixed((uint32_t)(uint16_t)pl.a1_q, 16);
    e.fixed(pl.step_q, 16);
    e.fixed(bits, 8);

    double x0 = (quant_fixed(s[0], 32768.0) / 32768.0 - pl.mean) / pl.peak;
    double xh = x0;
    e.fixed((uint32_t)(uint16_t)quant_clamp(x0, 32768.0, -32768.0, 32767.0), 16);

    int16_t a1_q_cur = pl.a1_q;
    double a1 = pl.a1;
    size_t severe = 0;

    for (size_t i = 1; i < n; i++) {
        if (block_len != 0 && (i % (size_t)block_len) == 0) {
            int upd = 0;
            if (policy == ACP2_ALWAYS) {
                upd = 1;
            } else if (policy == ACP2_DRIFT) {
                size_t need = ((size_t)block_len * (size_t)ACP2_DRIFT_PCT) / 100u;
                if (need == 0)
                    need = 1;
                upd = (severe >= need);
            }
            e.bit(&m.p_flag, upd);
            if (upd) {
                size_t jb = i - (size_t)block_len;
                if (jb == 0)
                    jb = 1;
                a1_q_cur = refit_a1_q(s, jb, i, pl.mean, pl.peak, a1_q_cur);
                e.fixed((uint32_t)(uint16_t)a1_q_cur, 16);
                a1 = (double)a1_q_cur / 16384.0;
            }
            severe = 0;
        }

        double src = (s[i] - pl.mean) / pl.peak;
        double pred = a1 * xh;
        int32_t q = (int32_t)quant_fixed((src - pred) / pl.step, 1.0);
        if (q > lim - 1)
            q = lim - 1;
        if (q < -lim)
            q = -lim;
        if (policy == ACP2_DRIFT && block_len != 0) {
            int32_t aq = (q < 0) ? -q : q;
            if (aq * ACP2_BASKET_DEN >= lim)
                severe++;
        }
        enc_sym(&e, &m, q);
        xh = pred + (double)q * pl.step;
    }

    e.done();
    out->swap(e.out);
    return ERR_OK;
}

/* NFA3: same chunk header and same DPCM state as the NFA1 path, but each
   symbol is split into a basket index (arithmetic coded from the basket
   histogram) and the leftover mantissa bits, and a saturated residual is
   escaped to a full 32 bit raw sample. */
Err encode_chunk3(const float* s, size_t n, const ChanPlan& pl,
                  uint16_t bits, std::vector<uint8_t>* out)
{
    int32_t lim = (int32_t)((1u << (bits - 1)) - 1u);
    int classes = (int)bits + 1;
    BasketModel bm;
    basket_init(&bm);

    EncCtx e;
    e.fixed(pl.peak_q, 16);
    e.fixed((uint32_t)(uint16_t)pl.mean_q, 16);
    e.fixed((uint32_t)(uint16_t)pl.a1_q, 16);
    e.fixed(pl.step_q, 16);
    e.fixed(bits, 8);

    double x0 = (quant_fixed(s[0], 32768.0) / 32768.0 - pl.mean) / pl.peak;
    double xh = x0;
    e.fixed((uint32_t)(uint16_t)quant_clamp(x0, 32768.0, -32768.0, 32767.0), 16);

    double a1 = pl.a1;
    SymModel z;
    sym_init(&z);
    int ctx = 0;

    for (size_t i = 1; i < n; i++) {
        double src = (s[i] - pl.mean) / pl.peak;
        double pred = a1 * xh;
        int32_t q = (int32_t)quant_fixed((src - pred) / pl.step, 1.0);
        if (q > lim - 1)
            q = lim - 1;
        if (q < -lim)
            q = -lim;

        int cl = basket_class(q, lim, classes);
        e.multi(bm.cnt[ctx], classes, bm.tot[ctx], cl);
        if (cl == classes - 1) {
            int32_t raw = quant_clamp(src, 32768.0, -32768.0, 32767.0);
            e.fixed((uint32_t)raw, 32);
            xh = (double)raw / 32768.0;
        } else {
            if (cl != 0) {
                e.bit(&z.p_sign, q < 0 ? 1 : 0);
                uint32_t mag = (q < 0) ? (uint32_t)(-q) : (uint32_t)q;
                for (int j = cl - 2; j >= 0; j--)
                    e.fixed((mag >> j) & 1u, 1);
            }
            xh = pred + (double)q * pl.step;
        }
        basket_bump(&bm, ctx, cl);
        ctx = cl;
    }

    e.done();
    out->swap(e.out);
    return ERR_OK;
}

/* ceil(v / d) for v >= 0 and d >= 1, the basket window boundary in sample
   units.  Callers guard d > 0 (a single-basket grid never reconstructs). */
int64_t ceil_div(int64_t v, int64_t d)
{
    return (v + d - 1) / d;
}
/*
 * NFA4, the basket codec: no predictor and no residual, just one basket symbol
 * per sample coded from an adaptive histogram that updates after every sample.
 * So that a basket can be a plain magnitude class with no sign symbol, the
 * channel grid is shifted first: the header carries the channel minimum base_q,
 * every sample is mapped to u = xq - base_q >= 0, and the decoder adds base_q
 * back.  On the shifted grid u is split into `nb` equal baskets of the channel
 * range peak_q, with factor = nb - 1:
 *   - b = floor(u * factor / peak_q)
 *   - the decoder reconstructs the basket midpoint
 *       mid_b = (ceil(b*peak_q/factor) + ceil((b+1)*peak_q/factor)) / 2
 *     and returns (base_q + mid_b) / 32768
 * The header stores nb itself, so the grid is any requested basket count up to
 * ACPCM4_MAX_BUCKETS rather than the 101 rows an integer-percent step caps at.
 * The histogram starts at zero on both sides and is updated after every sample,
 * so no table is transmitted.  A flat channel has peak_q == 0 and decodes
 * exactly: every sample lands in bucket 0, whose midpoint is 0.
 */

Err encode_chunk4(const float* s, size_t n, uint16_t bits, uint32_t nb,
                   std::vector<uint8_t>* out)
{
    (void)bits;   /* the basket grid is set by nb; bits is kept for the header */
    if (nb < ACPCM4_BASKETS_MIN || nb > ACPCM4_MAX_BUCKETS)
        return ERR_RANGE;

    /* Reject NaN and out-of-range first; the header fields are 1/32768 over
       a nominal [-2, 2] signal. */
    for (size_t i = 0; i < n; i++) {
        double a = s[i] < 0.0 ? -s[i] : s[i];
        if (!(a == a) || a > PEAK_LIMIT)
            return ERR_BAD_INPUT;
    }

    /* Shift the channel so the minimum sits at zero: store the min as the
       header base and measure the basket against the shifted peak. */
    int32_t lo_q = quant_clamp(s[0], 32768.0, -65536.0, 65535.0);
    int32_t hi_q = lo_q;
    for (size_t i = 1; i < n; i++) {
        int32_t xq = quant_clamp(s[i], 32768.0, -65536.0, 65535.0);
        if (xq < lo_q)
            lo_q = xq;
        if (xq > hi_q)
            hi_q = xq;
    }
    int32_t base_q = lo_q;
    int64_t peak_q = (int64_t)hi_q - (int64_t)lo_q;
    int64_t factor = (int64_t)nb - 1;

    EncCtx e;
    e.fixed((uint32_t)peak_q, 32);
    e.fixed((uint32_t)base_q, 32);
    e.fixed(bits, 8);
    e.fixed(nb, 16);

    BasketTree bt;
    bt.init((int)nb);
    for (size_t i = 0; i < n; i++) {
        int32_t xq = quant_clamp(s[i], 32768.0, -65536.0, 65535.0);
        int64_t u = (int64_t)xq - (int64_t)base_q;
        int b = 0;
        if (peak_q > 0 && factor > 0)
            b = (int)((u * factor) / peak_q);
        if (b >= (int)nb)
            b = (int)nb - 1;
        if (b < 0)
            b = 0;
        e.multi_cum(bt.pref(b), (int64_t)bt.cnt[b] + 1,
                    bt.tot + (int64_t)bt.M);
        bt.bump(b);
        if (bt.tot > (int64_t)(1 << 20))
            bt.rescale();
    }

    e.done();
    out->swap(e.out);
    return ERR_OK;
}


} /* namespace */

static Err acpcm_encode_impl(const char* path, const pcm_buf* p, uint16_t bits,
                             uint32_t block_len, acpcm_p2_policy policy,
                             bool v3, uint32_t step, uint32_t baskets)
{
    if (p == NULL || p->samples == NULL || p->channels == 0)
        return ERR_BAD_ARGS;
    if (bits < ACPCM_BITS_MIN || bits > ACPCM_BITS_MAX)
        return ERR_RANGE;

    size_t ch = p->channels;
    size_t frames = p->count / ch;
    if (frames == 0)
        return ERR_BAD_ARGS;
    if (frames > 0xFFFFFFFFull)
        return ERR_RANGE;

    /* Gather each channel so the encoder and the analyser agree on the order
       samples are visited in; a stride parameter here is an easy way to get
       a subtly different plan from the one that was measured. */
    std::vector<std::vector<uint8_t> > chunks(ch);
    bool nfa4 = (step != 0 || baskets != 0);
    uint32_t nb4 = 0;
    if (nfa4) {
        /* A direct basket count wins; otherwise map the step percent, which
           the integer-percent grid caps at 101 rows for step = 1. */
        uint32_t n = baskets;
        if (n == 0)
            n = (step > 100u) ? 1u : (100u / step) + 1u;
        if (n < ACPCM4_BASKETS_MIN)
            n = ACPCM4_BASKETS_MIN;
        if (n > ACPCM4_MAX_BUCKETS)
            n = ACPCM4_MAX_BUCKETS;
        nb4 = n;
    }
    for (size_t c = 0; c < ch; c++) {
        std::vector<float> gath(frames);
        for (size_t i = 0; i < frames; i++)
            gath[i] = p->samples[i * ch + c];
        Err e;
        if (nfa4) {
            /* NFA4 plans its own shifted grid, so skip the predictor plan */
            e = encode_chunk4(gath.data(), frames, bits, nb4, &chunks[c]);
        } else {
            ChanPlan pl;
            e = plan_channel(gath.data(), frames, bits, &pl);
            if (e != ERR_OK)
                return e;
            if (v3)
                e = encode_chunk3(gath.data(), frames, pl, bits, &chunks[c]);
            else
                e = encode_chunk(gath.data(), frames, pl, bits, &chunks[c],
                                 block_len, policy);
        }
        if (e != ERR_OK)
            return e;
    }

    uint64_t payload = 0;
    for (size_t c = 0; c < ch; c++)
        payload += chunks[c].size();

    int v2 = (block_len != 0);
    const char* magic = nfa4 ? ACPCM_MAGIC4
                     : (v3 ? ACPCM_MAGIC3 : (v2 ? ACPCM_MAGIC2 : ACPCM_MAGIC));
    size_t base = v2 ? (size_t)ACPCM_FIXED_HEADER2 : (size_t)ACPCM_FIXED_HEADER;
    size_t hsz = base + 4 * ch;
    std::vector<uint8_t> hdr(hsz, 0);
    memcpy(hdr.data(), magic, 4);
    put_u32(hdr.data() + 4, (uint32_t)frames);
    put_u32(hdr.data() + 8, (uint32_t)bits);
    put_u32(hdr.data() + 12, (uint32_t)ch);
    put_u32(hdr.data() + 16, p->sample_rate);
    if (v2)
        put_u32(hdr.data() + ACPCM_FIXED_HEADER, block_len);
    for (size_t c = 0; c < ch; c++)
        put_u32(hdr.data() + base + 4 * c, (uint32_t)chunks[c].size());

    FILE* f = fopen(path, "wb");
    if (f == NULL)
        return ERR_OPEN;
    Err rc = ERR_OK;
    if (fwrite(hdr.data(), 1, hdr.size(), f) != hdr.size())
        rc = ERR_IO;
    for (size_t c = 0; c < ch && rc == ERR_OK; c++)
        if (fwrite(chunks[c].data(), 1, chunks[c].size(), f) != chunks[c].size())
            rc = ERR_IO;
    if (fclose(f) != 0 && rc == ERR_OK)
        rc = ERR_IO;
    return rc;
}

Err acpcm_encode(const char* path, const pcm_buf* p, uint16_t bits)
{
    return acpcm_encode_impl(path, p, bits, 0, ACP2_NEVER, false, 0, 0);
}

Err acpcm_encode2(const char* path, const pcm_buf* p, uint16_t bits,
                  uint32_t block_len, acpcm_p2_policy policy)
{
    if (block_len == 0)
        return ERR_RANGE;
    if ((int)policy < (int)ACP2_NEVER || (int)policy > (int)ACP2_DRIFT)
        return ERR_RANGE;
    return acpcm_encode_impl(path, p, bits, block_len, policy, false, 0, 0);
}

Err acpcm_encode3(const char* path, const pcm_buf* p, uint16_t bits)
{
    return acpcm_encode_impl(path, p, bits, 0, ACP2_NEVER, true, 0, 0);
}

Err acpcm_encode4(const char* path, const pcm_buf* p, uint16_t bits,
                  uint32_t step)
{
    if (step < ACPCM4_STEP_MIN || step > ACPCM4_STEP_MAX)
        return ERR_RANGE;
    return acpcm_encode_impl(path, p, bits, 0, ACP2_NEVER, false, step, 0);
}

Err acpcm_encode4_baskets(const char* path, const pcm_buf* p, uint16_t bits,
                          uint32_t baskets)
{
    if (baskets < ACPCM4_BASKETS_MIN || baskets > ACPCM4_MAX_BUCKETS)
        return ERR_RANGE;
    return acpcm_encode_impl(path, p, bits, 0, ACP2_NEVER, false, 0, baskets);
}

Err acpcm_file_info(const char* path, acpcm_info* out)
{
    if (out == NULL)
        return ERR_BAD_ARGS;

    FILE* f = fopen(path, "rb");
    if (f == NULL)
        return ERR_OPEN;

    uint8_t mag[4];
    if (fread(mag, 1, 4, f) != 4 ||
        (memcmp(mag, ACPCM_MAGIC, 4) != 0 &&
         memcmp(mag, ACPCM_MAGIC2, 4) != 0 &&
         memcmp(mag, ACPCM_MAGIC3, 4) != 0 &&
         memcmp(mag, ACPCM_MAGIC4, 4) != 0)) {
        fclose(f);
        return ERR_BAD_NOFFT;
    }
    int fmt = (memcmp(mag, ACPCM_MAGIC4, 4) == 0) ? 3
            : (memcmp(mag, ACPCM_MAGIC3, 4) == 0) ? 2
            : (memcmp(mag, ACPCM_MAGIC2, 4) == 0) ? 1 : 0;
    int v2 = (fmt == 1);

    uint8_t rest[16]; /* frames, bits, channels, sample_rate */
    if (fread(rest, 1, sizeof(rest), f) != sizeof(rest)) {
        fclose(f);
        return ERR_BAD_NOFFT;
    }

    uint32_t frames = get_u32(rest + 0);
    uint32_t bits = get_u32(rest + 4);
    uint32_t ch = get_u32(rest + 8);
    uint32_t rate = get_u32(rest + 12);
    if (ch == 0 || ch > 64 || rate == 0 ||
        bits < ACPCM_BITS_MIN || bits > ACPCM_BITS_MAX) {
        fclose(f);
        return ERR_BAD_NOFFT;
    }

    uint32_t block_len = 0;
    if (v2) {
        uint8_t blb[4];
        if (fread(blb, 1, 4, f) != 4) {
            fclose(f);
            return ERR_BAD_NOFFT;
        }
        block_len = get_u32(blb);
        if (block_len == 0) { /* NFA2 magic without a block: not a valid file */
            fclose(f);
            return ERR_BAD_NOFFT;
        }
    }

    uint8_t lenbuf[256];
    if (ch > 64 || fread(lenbuf, 1, 4 * ch, f) != 4 * ch) {
        fclose(f);
        return ERR_BAD_NOFFT;
    }

    uint64_t payload = 0;
    for (uint32_t c = 0; c < ch; c++)
        payload += get_u32(lenbuf + 4 * c);

    out->bits = (uint16_t)bits;
    out->channels = (uint16_t)ch;
    out->sample_rate = rate;
    out->block_len = block_len;
    out->format = (uint8_t)fmt;
    out->frames = frames;
    out->payload = payload;
    out->total = (v2 ? ACPCM_FIXED_HEADER2 : ACPCM_FIXED_HEADER) +
                 (uint64_t)4 * ch + payload;

    fclose(f);
    return ERR_OK;
}

struct DecChan {
    DecCtx rc;
    SymModel m;
    BasketModel bm;
    double xh;
    double peak;
    double mean;
    double a1;
    double step;
    int32_t lim;
    int classes;   /* NFA3: basket alphabet size, bits+1 */
    int ctx;       /* NFA3: previous sample's basket, the histogram row */
    int fmt;       /* 0: NFA1, 1: NFA2, 2: NFA3, 3: NFA4 */
    bool primed;
    uint32_t block_len;  /* 0: NFA1/NFA3/NFA4, no flags */
    uint32_t in_block;   /* samples since the last boundary */

    /* NFA4: adaptive basket histogram, updated after every sample, indexed by a
       Fenwick tree so the symbol search is O(log M). */
    BasketTree bt4;
    int64_t factor4;    /* bt4.M - 1, baskets across the shifted range */
    int64_t peak4;      /* shifted channel range peak_q */
    int32_t base4;      /* signed grid offset on the 1/32768 scale */

    Err init(const uint8_t* d, size_t n, uint16_t bits, uint32_t bl, int fmt)
    {
        rc.d = d;
        rc.n = n;
        rc.pos = 0;
        rc.range = 0xFFFFFFFFu;
        rc.code = 0;
        for (int i = 0; i < 5; i++)
            rc.code = (rc.code << 8) | (uint32_t)(rc.pos < n ? d[rc.pos++] : 0u);

        this->fmt = fmt;
        if (fmt == 3)
            return init4(bits);

        uint32_t peak_q = rc.fixed(16);
        int32_t mean_q = (int32_t)(int16_t)(uint16_t)rc.fixed(16);
        int32_t a1_q = (int32_t)(int16_t)(uint16_t)rc.fixed(16);
        uint32_t step_q = rc.fixed(16);
        uint32_t hb = rc.fixed(8);

        if (hb != (uint32_t)bits || peak_q == 0 || step_q == 0)
            return ERR_BAD_NOFFT;

        peak = (double)peak_q / 32768.0;
        mean = (double)mean_q / 32768.0;
        a1 = (double)a1_q / 16384.0;
        step = (double)step_q / 32768.0;
        lim = (int32_t)((1u << (bits - 1)) - 1u);
        sym_init(&m);
        classes = (int)bits + 1;
        ctx = 0;
        if (fmt == 2)
            basket_init(&bm);
        block_len = bl;
        in_block = 0;

        double x0 = (double)(int32_t)(int16_t)(uint16_t)rc.fixed(16) / 32768.0;
        xh = x0;
        primed = false;
        return ERR_OK;
    }

    /* NFA4 chunk: the shifted-grid peak and base, and the basket count.  The
       basket histogram is adaptive (starts at zero, updates after every
       sample), so no table is transmitted.  Every sample is coded, so the
       frame count is not stored either. */
    Err init4(uint16_t bits)
    {
        uint32_t pq = rc.fixed(32);
        int32_t bq = (int32_t)rc.fixed(32);
        uint32_t hb = rc.fixed(8);
        uint32_t nb = rc.fixed(16);

        if (hb != (uint32_t)bits ||
            nb < ACPCM4_BASKETS_MIN || nb > ACPCM4_MAX_BUCKETS)
            return ERR_BAD_NOFFT;

        base4 = bq;
        peak4 = (int64_t)pq;
        bt4.init((int)nb);
        factor4 = (int64_t)bt4.M - 1;
        block_len = 0;
        in_block = 0;
        primed = true;   /* NFA4 has no seed sample; every sample is coded */
        return ERR_OK;
    }

    double next()
    {
        if (!primed) {
            primed = true;
            in_block = 1;
            return xh * peak + mean;
        }
        if (fmt == 3) {
            int b = rc.multi_tree(&bt4);
            int64_t out_q = (int64_t)base4;
            if (peak4 > 0 && factor4 > 0) {
                int64_t lo = ceil_div((int64_t)b * peak4, factor4);
                int64_t hi = ceil_div((int64_t)(b + 1) * peak4, factor4);
                out_q += (lo + hi) / 2;
            }
            if (out_q < -65536)
                out_q = -65536;
            if (out_q > 65535)
                out_q = 65535;
            bt4.bump(b);
            if (bt4.tot > (int64_t)(1 << 20))
                bt4.rescale();
            in_block++;
            return (double)out_q / 32768.0;
        }
        if (fmt == 2) {
            int cl = rc.multi(bm.cnt[ctx], classes, bm.tot[ctx]);
            int32_t q = 0;
            if (cl == classes - 1) {
                int32_t raw = (int32_t)rc.fixed(32);
                xh = (double)raw / 32768.0;
                basket_bump(&bm, ctx, cl);
                ctx = cl;
                in_block++;
                return xh * peak + mean;
            }
            if (cl != 0) {
                int neg = rc.bit(&m.p_sign);
                uint32_t mag = 1;
                for (int j = cl - 2; j >= 0; j--)
                    mag = (mag << 1) | rc.fixed(1);
                q = neg ? -(int32_t)mag : (int32_t)mag;
            }
            basket_bump(&bm, ctx, cl);
            ctx = cl;
            double pred = a1 * xh;
            xh = pred + (double)q * step;
            in_block++;
            return xh * peak + mean;
        }
        if (block_len != 0 && in_block == block_len) {
            in_block = 0;
            if (rc.bit(&m.p_flag) != 0) {
                int32_t nq = (int32_t)(int16_t)(uint16_t)rc.fixed(16);
                a1 = (double)nq / 16384.0;
            }
        }
        int32_t q = dec_sym(&rc, &m);
        if (q > lim - 1)
            q = lim - 1;
        if (q < -lim)
            q = -lim;
        double pred = a1 * xh;
        xh = pred + (double)q * step;
        in_block++;
        return xh * peak + mean;
    }
};

struct AcpcmDecoder {
    acpcm_info info;
    std::vector<std::vector<uint8_t> > raw;
    std::vector<DecChan> ch;
    size_t frames;
    size_t pos;
};

Err acpcm_dec_open(const char* path, AcpcmDecoder** out)
{
    *out = NULL;

    AcpcmDecoder* d = new (std::nothrow) AcpcmDecoder();
    if (d == NULL)
        return ERR_NO_MEMORY;

    Err e = acpcm_file_info(path, &d->info);
    if (e != ERR_OK) {
        delete d;
        return e;
    }

    size_t ch = d->info.channels;
    size_t frames = (size_t)d->info.frames;
    if (frames == 0) {
        delete d;
        return ERR_BAD_NOFFT;
    }

    FILE* f = fopen(path, "rb");
    if (f == NULL) {
        delete d;
        return ERR_OPEN;
    }

    d->raw.assign(ch, std::vector<uint8_t>());
    d->ch.assign(ch, DecChan());
    d->frames = frames;
    d->pos = 0;

    e = ERR_OK;
    std::vector<uint8_t> lenbuf(4 * ch);
    size_t lbase = (d->info.block_len != 0) ? (size_t)ACPCM_FIXED_HEADER2
                                            : (size_t)ACPCM_FIXED_HEADER;
    if (fseek(f, (long)lbase, SEEK_SET) != 0 ||
        fread(lenbuf.data(), 1, lenbuf.size(), f) != lenbuf.size()) {
        e = ERR_IO;
    }

    for (size_t c = 0; c < ch && e == ERR_OK; c++) {
        uint32_t len = get_u32(lenbuf.data() + 4 * c);
        if (len == 0) {
            e = ERR_BAD_NOFFT;
            break;
        }
        d->raw[c].resize(len);
        if (fread(d->raw[c].data(), 1, len, f) != len)
            e = ERR_IO;
    }
    fclose(f);

    for (size_t c = 0; c < ch && e == ERR_OK; c++)
        e = d->ch[c].init(d->raw[c].data(), d->raw[c].size(), d->info.bits,
                          d->info.block_len, d->info.format);

    if (e != ERR_OK) {
        delete d;
        return e;
    }

    *out = d;
    return ERR_OK;
}

void acpcm_dec_close(AcpcmDecoder* d)
{
    delete d;
}

const acpcm_info* acpcm_dec_info(const AcpcmDecoder* d)
{
    return (d == NULL) ? NULL : &d->info;
}

Err acpcm_dec_read(AcpcmDecoder* d, float* out, size_t max_frames, size_t* got)
{
    if (d == NULL || out == NULL || got == NULL)
        return ERR_BAD_ARGS;

    size_t ch = d->info.channels;
    size_t done = 0;
    while (done < max_frames && d->pos < d->frames) {
        for (size_t c = 0; c < ch; c++)
            out[done * ch + c] = (float)d->ch[c].next();
        done++;
        d->pos++;
    }
    *got = done;
    return ERR_OK;
}

Err acpcm_decode(const char* path, pcm_buf* out)
{
    memset(out, 0, sizeof(*out));

    AcpcmDecoder* d = NULL;
    Err e = acpcm_dec_open(path, &d);
    if (e != ERR_OK)
        return e;

    size_t ch = d->info.channels;
    size_t frames = d->frames;
    if (frames > (SIZE_MAX / sizeof(float)) / (ch == 0 ? 1 : ch)) {
        acpcm_dec_close(d);
        return ERR_NO_MEMORY;
    }

    float* buf = (float*)malloc(frames * ch * sizeof(float));
    if (buf == NULL) {
        acpcm_dec_close(d);
        return ERR_NO_MEMORY;
    }

    size_t done = 0;
    while (done < frames) {
        size_t got = 0;
        e = acpcm_dec_read(d, buf + done * ch, frames - done, &got);
        if (e != ERR_OK || got == 0) {
            free(buf);
            acpcm_dec_close(d);
            return (e == ERR_OK) ? ERR_BAD_NOFFT : e;
        }
        done += got;
    }
    uint32_t rate = d->info.sample_rate;
    uint16_t chans = (uint16_t)ch;
    acpcm_dec_close(d);

    out->samples = buf;
    out->count = frames * ch;
    out->channels = chans;
    out->sample_rate = rate;
    out->sample_rate = rate;
    return ERR_OK;
}


#ifdef HAVE_ALSA
#include <alsa/asoundlib.h>
#endif

#define ACPCM_BLOCK_FRAMES 2048

#ifndef HAVE_ALSA

Err acpcm_play(const char* path, const char* device)
{
    (void)path;
    (void)device;
    return ERR_UNSUPPORTED_FMT;
}

#else

static int alsa_write_all(snd_pcm_t* pcm, const float* buf,
                          snd_pcm_uframes_t frames, unsigned chans)
{
    const uint8_t* p = (const uint8_t*)buf;
    snd_pcm_uframes_t left = frames;
    while (left > 0) {
        snd_pcm_sframes_t n = snd_pcm_writei(pcm, p, left);
        if (n < 0) {
            int r = snd_pcm_recover(pcm, (int)n, 0);
            if (r < 0)
                return -1;
            continue;
        }
        p += (size_t)n * chans * 4u;
        left -= (snd_pcm_uframes_t)n;
    }
    return 0;
}

Err acpcm_play(const char* path, const char* device)
{
    AcpcmDecoder* d = NULL;
    Err e = acpcm_dec_open(path, &d);
    if (e != ERR_OK)
        return e;

    snd_pcm_t* pcm = NULL;
    int err = snd_pcm_open(&pcm, (device != NULL) ? device : "default",
                           SND_PCM_STREAM_PLAYBACK, 0);
    if (err < 0) {
        acpcm_dec_close(d);
        return ERR_OPEN;
    }

    snd_pcm_hw_params_t* hw = NULL;
    snd_pcm_hw_params_malloc(&hw);
    if (hw == NULL) {
        snd_pcm_close(pcm);
        acpcm_dec_close(d);
        return ERR_NO_MEMORY;
    }

    snd_pcm_hw_params_any(pcm, hw);
    unsigned rate = d->info.sample_rate;
    unsigned chans = d->info.channels;
    snd_pcm_hw_params_set_access(pcm, hw, SND_PCM_ACCESS_RW_INTERLEAVED);
    snd_pcm_hw_params_set_format(pcm, hw, SND_PCM_FORMAT_FLOAT_LE);
    snd_pcm_hw_params_set_channels(pcm, hw, chans);
    snd_pcm_hw_params_set_rate_near(pcm, hw, &rate, NULL);

    if (snd_pcm_hw_params(pcm, hw) < 0) {
        snd_pcm_hw_params_free(hw);
        snd_pcm_close(pcm);
        acpcm_dec_close(d);
        return ERR_UNSUPPORTED_FMT;
    }
    snd_pcm_hw_params_free(hw);

    float* block = (float*)malloc((size_t)ACPCM_BLOCK_FRAMES * chans * sizeof(float));
    if (block == NULL) {
        snd_pcm_close(pcm);
        acpcm_dec_close(d);
        return ERR_NO_MEMORY;
    }

    for (;;) {
        size_t got = 0;
        e = acpcm_dec_read(d, block, ACPCM_BLOCK_FRAMES, &got);
        if (e != ERR_OK) {
            free(block);
            snd_pcm_close(pcm);
            acpcm_dec_close(d);
            return e;
        }
        if (got == 0)
            break;
        if (alsa_write_all(pcm, block, (snd_pcm_uframes_t)got, chans) != 0) {
            free(block);
            snd_pcm_close(pcm);
            acpcm_dec_close(d);
            return ERR_IO;
        }
    }

    snd_pcm_drain(pcm);
    free(block);
    snd_pcm_close(pcm);
    acpcm_dec_close(d);
    return ERR_OK;
}

#endif

Err acpcm_gen(const char* path, double seconds, uint32_t channels, uint32_t sample_rate)
{
    if (seconds <= 0.0 || channels == 0 || channels > 8 || sample_rate == 0)
        return ERR_BAD_ARGS;

    size_t frames = (size_t)(seconds * (double)sample_rate);
    pcm_buf p;
    p.samples = (float*)malloc(frames * channels * sizeof(float));
    if (p.samples == NULL)
        return ERR_NO_MEMORY;
    p.count = frames * channels;
    p.channels = (uint16_t)channels;
    p.sample_rate = sample_rate;

    for (size_t i = 0; i < frames; i++) {
        double t = (double)i / (double)sample_rate;
        double a = 0.45 * sin(6.283185307179586 * 440.0 * t);
        double b = 0.30 * sin(6.283185307179586 * 661.0 * t);
        double f = 20.0 + (8000.0 - 20.0) * ((double)i / (double)frames);
        double c = 0.20 * sin(6.283185307179586 * f * t);
        for (uint32_t j = 0; j < channels; j++) {
            p.samples[i * channels + j] = (float)(a + b * (double)(j + 1) / (double)channels
                                                   + c * (double)(j + 1));
        }
    }

    Err e = wav_save(path, &p);
    pcm_free(&p);
    return e;
}
