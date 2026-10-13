#ifndef NOFFT_H
#define NOFFT_H

#include <stdint.h>
#include <stddef.h>
#include <stdio.h>

#define NOFFT_MAGIC   "NOFF"
#define NOFFT_VERSION 6
#define NOFFT_HEADER_SIZE 32

enum Err {
    ERR_OK = 0,
    ERR_IO,
    ERR_OPEN,
    ERR_NOT_WAV,
    ERR_UNSUPPORTED_FMT,
    ERR_NO_MEMORY,
    ERR_BAD_NOFFT,
    ERR_BAD_ARGS,
    ERR_RANGE,
    ERR_SINGULAR,
    ERR_BAD_INPUT,
    ERR_COUNT
};

typedef const char* (*err_str_fn)(Err);

extern err_str_fn g_err_str;

typedef enum {
    NOFFT_COEF_F32 = 0,
    NOFFT_COEF_F16 = 1
} nofft_coeff_format;

/* how the polynomial for each block is chosen */
typedef enum {
    /* exact interpolation through picked samples (format 3 behaviour) */
    NOFFT_FIT_INTERP = 0,
    /* least squares over every sample in the block */
    NOFFT_FIT_LEAST_SQ = 1
} nofft_fit;

typedef struct {
    uint32_t sample_rate;
    uint16_t channels;
    uint16_t degree;
    uint32_t frame_len;
    uint32_t frame_count;
    uint32_t last_len;
    nofft_coeff_format coeff_format;
    nofft_fit fit;
} nofft_header;

typedef struct {
    float*    samples;
    size_t    count;
    uint32_t  sample_rate;
    uint16_t  channels;
} pcm_buf;

void pcm_free(pcm_buf* p);

Err wav_load(const char* path, pcm_buf* out);
Err wav_save(const char* path, const pcm_buf* p);
Err wav_write_stream(FILE* out, const pcm_buf* p);

Err nofft_encode(const char* path, const pcm_buf* p, uint32_t frame_len, uint16_t degree,
                 nofft_coeff_format cf, nofft_fit fit);
Err nofft_decode(const char* path, pcm_buf* out);

/* Reads only the header of a .no_fft file and reports which fit was used.
   Version 2 and 3 files report NOFFT_FIT_INTERP. */
Err nofft_file_fit(const char* path, nofft_fit* out_fit);

const char* nofft_coeff_format_str(nofft_coeff_format cf);
int nofft_coeff_format_parse(const char* s, nofft_coeff_format* out);
const char* nofft_fit_str(nofft_fit f);
int nofft_fit_parse(const char* s, nofft_fit* out);

float nofft_decode_snr_db(const float* a, const float* b, size_t n);

Err nofft_solve_vandermonde(const double* xs, const double* ys, int n, double* out_coefs);
double nofft_eval_poly(const double* c, int n, double x);

/* Least-squares polynomial fit of degree n-1 to (xs, ys), rows >= n.
   Solves min ||A x - ys|| by Householder QR, which is what makes high
   degrees usable: the normal equations square the condition number and lose
   all precision by around n=20 on this Vandermonde. */
Err nofft_solve_least_squares(const double* a, const double* ys, int rows, int n,
                              double* out_coefs);

uint16_t nofft_effective_degree(uint16_t degree, uint32_t len);
double  nofft_subrange_mean(const float* s, size_t n, size_t stride);
void    nofft_subrange_pick(const float* s, size_t n, size_t stride, double mean, uint32_t* out_index, double* out_value);

#endif