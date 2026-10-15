#ifndef NFM_H
#define NFM_H

/* nfm - No_FFT Media container.
 *
 * Wraps one compressed audio payload (any No_FFT codec) with optional text
 * metadata and an optional image. The format is little-endian and
 * length-prefixed throughout, so a decoder can skip the header without
 * scanning for a marker.
 *
 * Layout:
 *   magic[3]      "nfm"
 *   header_len    u64   length of the header that follows
 *   header data:  header_len bytes:
 *     image:      img_type u8, img_len u32, img_data[]   (img_len 0 = none)
 *     text fields:name_len u16, name[], value_len u32, value[]   (0+ fields)
 *   payload       raw codec bytes (NOFF / NFA1..NFA6 magic)
 */

#include <stddef.h>
#include <stdint.h>

#include "nofft.h"

#ifdef __cplusplus
extern "C" {
#endif

#define NFM_MAGIC "nfm"
#define NFM_VERSION 1u

/* image types */
enum {
    NFM_IMG_NONE = 0,
    NFM_IMG_PNG  = 1,
    NFM_IMG_JPEG = 2
};

typedef struct {
    const char* title;   /* NULL if absent */
    const char* artist;  /* NULL if absent */
    const char* album;   /* NULL if absent */
} nfm_meta;

/* Writes an NFM file: magic + header_len + header + payload.
   Any of title/artist/album/img may be NULL.  Returns ERR_OK on success. */
Err nfm_write(const char* path,
              const uint8_t* payload, size_t payload_len,
              const nfm_meta* meta,
              const uint8_t* img, size_t img_len, int img_type);

/* Reads an NFM file.  On success, the payload and payload_len outputs hold
   the inner codec bytes (caller releases with nfm_buf_free); meta (if non-NULL)
   is filled with pointers into an internal buffer that is valid until nfm_read
   is called again or the string is released with nfm_free_meta; the image
   outputs (if non-NULL) describe the image, or NFM_IMG_NONE. */
Err nfm_read(const char* path,
             uint8_t** payload, size_t* payload_len,
             nfm_meta* meta,
             uint8_t** img, size_t* img_len, int* img_type);

/* Detects NFM by magic (first 3 bytes). */
int nfm_is_nfm(const uint8_t* data, size_t len);

/* Releases strings returned by nfm_read (or NULL). */
void nfm_free_meta(nfm_meta* meta);

/* Releases buffers returned by nfm_read. */
void nfm_buf_free(void* p);

#ifdef __cplusplus
}
#endif

#endif
