/* nfm.cpp - No_FFT Media container implementation.
 *
 * The format is a length-prefixed header: magic "nfm", a u64 header_len,
 * then header_len bytes holding an image block (always present, possibly
 * empty) followed by text fields, then the raw codec payload.
 */

#include "nfm.h"

#include <stdlib.h>
#include <string.h>

/* ---- little-endian helpers ---- */

static void put_u16(uint8_t* d, uint16_t v)
{
    d[0] = (uint8_t)(v & 0xFF);
    d[1] = (uint8_t)((v >> 8) & 0xFF);
}

static uint16_t get_u16(const uint8_t* d)
{
    return (uint16_t)d[0] | ((uint16_t)d[1] << 8);
}

static void put_u32(uint8_t* d, uint32_t v)
{
    d[0] = (uint8_t)(v & 0xFF);
    d[1] = (uint8_t)((v >> 8) & 0xFF);
    d[2] = (uint8_t)((v >> 16) & 0xFF);
    d[3] = (uint8_t)((v >> 24) & 0xFF);
}

static uint32_t get_u32(const uint8_t* d)
{
    return (uint32_t)d[0] | ((uint32_t)d[1] << 8) |
           ((uint32_t)d[2] << 16) | ((uint32_t)d[3] << 24);
}

static void put_u64(uint8_t* d, uint64_t v)
{
    for (int i = 0; i < 8; i++)
        d[i] = (uint8_t)((v >> (8 * i)) & 0xFF);
}

static uint64_t get_u64(const uint8_t* d)
{
    uint64_t v = 0;
    for (int i = 7; i >= 0; i--)
        v = (v << 8) | d[i];
    return v;
}

/* Writes one text field: name_len u16, name, value_len u32, value. */
static int write_field(FILE* f, const char* name, const char* value)
{
    size_t name_len = strlen(name);
    size_t value_len = strlen(value);
    if (name_len > 0xFFFF || value_len > 0xFFFFFFFFull)
        return 0;

    uint8_t nl[2];
    put_u16(nl, (uint16_t)name_len);
    if (fwrite(nl, 1, 2, f) != 2) return 0;
    if (name_len > 0 && fwrite(name, 1, name_len, f) != name_len) return 0;

    uint8_t vl[4];
    put_u32(vl, (uint32_t)value_len);
    if (fwrite(vl, 1, 4, f) != 4) return 0;
    if (value_len > 0 && fwrite(value, 1, value_len, f) != value_len) return 0;
    return 1;
}

/* Computes the header size for the given metadata and image. */
static uint64_t header_size(const nfm_meta* meta,
                            const uint8_t* img, size_t img_len, int img_type)
{
    /* Image block is always present: img_type u8, img_len u32 (+ data). */
    uint64_t h = 1 + 4;
    if (img != NULL && img_len > 0 && img_type > NFM_IMG_NONE)
        h += img_len;

    if (meta != NULL && meta->title != NULL)
        h += 2 + strlen("title") + 4 + strlen(meta->title);
    if (meta != NULL && meta->artist != NULL)
        h += 2 + strlen("artist") + 4 + strlen(meta->artist);
    if (meta != NULL && meta->album != NULL)
        h += 2 + strlen("album") + 4 + strlen(meta->album);

    return h;
}

Err nfm_write(const char* path,
              const uint8_t* payload, size_t payload_len,
              const nfm_meta* meta,
              const uint8_t* img, size_t img_len, int img_type)
{
    if (path == NULL || payload == NULL || payload_len == 0)
        return ERR_BAD_ARGS;

    uint64_t hlen = header_size(meta, img, img_len, img_type);

    FILE* f = fopen(path, "wb");
    if (f == NULL)
        return ERR_OPEN;

    /* magic */
    int ok = (fwrite(NFM_MAGIC, 1, 3, f) == 3);

    /* header_len u64 */
    uint8_t hl[8];
    put_u64(hl, hlen);
    if (ok) ok = (fwrite(hl, 1, 8, f) == 8);

    /* image block (always present; img_len == 0 means no image) */
    int have_img = (img != NULL && img_len > 0 && img_type > NFM_IMG_NONE);
    uint8_t ih[5];
    ih[0] = (uint8_t)(have_img ? img_type : NFM_IMG_NONE);
    put_u32(ih + 1, (uint32_t)(have_img ? img_len : 0));
    if (ok) ok = (fwrite(ih, 1, 5, f) == 5);
    if (ok && have_img) ok = (fwrite(img, 1, img_len, f) == img_len);

    /* text fields */
    if (meta != NULL && meta->title  != NULL) { if (ok) ok = write_field(f, "title",  meta->title);  }
    if (meta != NULL && meta->artist != NULL) { if (ok) ok = write_field(f, "artist", meta->artist); }
    if (meta != NULL && meta->album  != NULL) { if (ok) ok = write_field(f, "album",  meta->album);  }

    /* payload */
    if (ok) ok = (fwrite(payload, 1, payload_len, f) == payload_len);

    if (fclose(f) != 0) ok = 0;
    return ok ? ERR_OK : ERR_IO;
}

/* Reads the whole file into a malloc'd buffer. */
static Err read_all(const char* path, uint8_t** out, size_t* out_len)
{
    if (path == NULL || out == NULL || out_len == NULL)
        return ERR_BAD_ARGS;
    *out = NULL;
    *out_len = 0;

    FILE* f = fopen(path, "rb");
    if (f == NULL)
        return ERR_OPEN;

    size_t cap = 1 << 16, len = 0;
    uint8_t* buf = (uint8_t*)malloc(cap);
    if (buf == NULL) { fclose(f); return ERR_NO_MEMORY; }

    for (;;) {
        if (len == cap) {
            size_t ncap = cap * 2;
            uint8_t* nb = (uint8_t*)realloc(buf, ncap);
            if (nb == NULL) { free(buf); fclose(f); return ERR_NO_MEMORY; }
            buf = nb; cap = ncap;
        }
        size_t got = fread(buf + len, 1, cap - len, f);
        len += got;
        if (got == 0) {
            if (ferror(f)) { free(buf); fclose(f); return ERR_IO; }
            break;
        }
    }
    fclose(f);
    *out = buf;
    *out_len = len;
    return ERR_OK;
}

Err nfm_read(const char* path,
             uint8_t** payload, size_t* payload_len,
             nfm_meta* meta,
             uint8_t** img, size_t* img_len, int* img_type)
{
    if (payload == NULL || payload_len == NULL)
        return ERR_BAD_ARGS;
    *payload = NULL;
    *payload_len = 0;
    if (img != NULL) *img = NULL;
    if (img_len != NULL) *img_len = 0;
    if (img_type != NULL) *img_type = NFM_IMG_NONE;
    if (meta != NULL) { meta->title = NULL; meta->artist = NULL; meta->album = NULL; }

    uint8_t* data = NULL;
    size_t len = 0;
    Err e = read_all(path, &data, &len);
    if (e != ERR_OK)
        return e;

    /* Need at least magic + header_len. */
    if (len < 3 + 8 || memcmp(data, NFM_MAGIC, 3) != 0) {
        free(data);
        return ERR_BAD_INPUT;
    }

    uint64_t hlen = get_u64(data + 3);
    size_t payload_off = 3 + 8 + hlen;
    if (payload_off > len) {
        free(data);
        return ERR_BAD_INPUT;
    }

    /* Parse header: image block first, then text fields. */
    size_t pos = 3 + 8;
    size_t hend = 3 + 8 + hlen;

    if (pos + 5 > hend) { free(data); return ERR_BAD_INPUT; }
    int itype = (int)data[pos];
    pos += 1;
    uint32_t ilen = get_u32(data + pos);
    pos += 4;
    if (pos + ilen > hend) { free(data); return ERR_BAD_INPUT; }
    if (img != NULL && ilen > 0) {
        *img = (uint8_t*)malloc(ilen);
        if (*img == NULL) { free(data); return ERR_NO_MEMORY; }
        memcpy(*img, data + pos, ilen);
        if (img_len != NULL) *img_len = ilen;
        if (img_type != NULL) *img_type = itype;
    }
    pos += ilen;

    /* Text fields. */
    while (pos < hend) {
        if (pos + 2 > hend) { free(data); return ERR_BAD_INPUT; }
        uint16_t name_len = get_u16(data + pos);
        pos += 2;
        if (pos + name_len + 4 > hend) { free(data); return ERR_BAD_INPUT; }
        const uint8_t* name = data + pos;
        uint32_t value_len = get_u32(data + pos + name_len);
        pos += name_len;
        pos += 4;
        if (pos + value_len > hend) { free(data); return ERR_BAD_INPUT; }

        /* Map known text fields. */
        if (name_len == 5 && memcmp(name, "title", 5) == 0) {
            if (meta != NULL && meta->title == NULL) {
                char* s = (char*)malloc(value_len + 1);
                if (s == NULL) { free(data); return ERR_NO_MEMORY; }
                memcpy(s, data + pos, value_len);
                s[value_len] = '\0';
                meta->title = s;
            }
        } else if (name_len == 6 && memcmp(name, "artist", 6) == 0) {
            if (meta != NULL && meta->artist == NULL) {
                char* s = (char*)malloc(value_len + 1);
                if (s == NULL) { free(data); return ERR_NO_MEMORY; }
                memcpy(s, data + pos, value_len);
                s[value_len] = '\0';
                meta->artist = s;
            }
        } else if (name_len == 5 && memcmp(name, "album", 5) == 0) {
            if (meta != NULL && meta->album == NULL) {
                char* s = (char*)malloc(value_len + 1);
                if (s == NULL) { free(data); return ERR_NO_MEMORY; }
                memcpy(s, data + pos, value_len);
                s[value_len] = '\0';
                meta->album = s;
            }
        }

        pos += value_len;
    }

    /* Payload is everything after the header. */
    *payload_len = len - payload_off;
    *payload = (uint8_t*)malloc(*payload_len > 0 ? *payload_len : 1);
    if (*payload == NULL) { free(data); return ERR_NO_MEMORY; }
    if (*payload_len > 0)
        memcpy(*payload, data + payload_off, *payload_len);

    free(data);
    return ERR_OK;
}

int nfm_is_nfm(const uint8_t* data, size_t len)
{
    return (data != NULL && len >= 3 && memcmp(data, NFM_MAGIC, 3) == 0);
}

void nfm_free_meta(nfm_meta* meta)
{
    if (meta == NULL)
        return;
    free((void*)meta->title);
    free((void*)meta->artist);
    free((void*)meta->album);
    meta->title = meta->artist = meta->album = NULL;
}

void nfm_buf_free(void* p)
{
    free(p);
}
