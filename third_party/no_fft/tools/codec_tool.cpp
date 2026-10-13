/* codec_tool.cpp - a small host for the codec modules.
 *
 * This is the "other application" the module layer exists for.  It links the
 * core library, autoloads the builtins plus any modules on NOFFT_MODULE_PATH,
 * and then drives codecs entirely through the nofft_codec vtable - it never
 * names a codec function directly, so a codec that only ever arrives as a
 * loadable module works exactly like one compiled into the library.
 *
 *   nofft_codec list
 *   nofft_codec info  <codec|->      <in>
 *   nofft_codec encode <codec> <in.wav> <out> [key=value ...]
 *   nofft_codec decode <codec> <in>     <out.wav>
 *   nofft_codec roundtrip <codec> <in.wav>    [key=value ...]
 *   nofft_codec load <module.so>
 *   nofft_codec dir  <dir>
 *
 * keys: bits block step baskets degree frame coef fit e err rate policy auto
 */

#include "codec.h"
#include "nofft.h"

#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>

static double compute_snr(const float* a, const float* b, size_t n)
{
    double num = 0.0, den = 0.0;
    for (size_t i = 0; i < n; i++) {
        double e = (double)a[i] - (double)b[i];
        double s = (double)a[i];
        num += e * e;
        den += s * s;
    }
    if (den <= 0.0) return 999.0;
    if (num <= 0.0) return 999.0;
    return 10.0 * log10(den / num);
}

static void usage(const char* argv0)
{
    fprintf(stderr,
        "usage:\n"
        "  %s list | count\n"
        "  %s info  <codec|->      <in>\n"
        "  %s encode <codec> <in.wav> <out> [key=value ...]\n"
        "  %s decode <codec> <in>     <out.wav>\n"
        "  %s roundtrip <codec> <in.wav>    [key=value ...]\n"
        "  %s load <module.so>\n"
        "  %s dir  <dir>\n"
        "\n"
        "keys: bits block step baskets degree frame coef fit e err rate policy auto\n"
        "modules are searched on NOFFT_MODULE_PATH (dirs split by '%c');\n"
        "set NOFFT_NO_BUILTINS=1 to register only the loadable modules\n",
        argv0, argv0, argv0, argv0, argv0, argv0, argv0,
#if defined(_WIN32)
        ';'
#else
        ':'
#endif
        );
}

/* Applies one key=value pair to the option set.  Unknown keys are an error so
   a typo fails loudly instead of silently encoding with the default. */
static int set_opt(nofft_codec_opts* o, const char* key, const char* val)
{
    double d = 0.0;
    long l = 0;

    if (strcmp(key, "auto") == 0) {
        o->flags |= NOFFT_OPT_AUTO;
        return 0;
    }
    if (strcmp(key, "coef") == 0) {
        o->coef_format = (strcmp(val, "f16") == 0) ? NOFFT_COEF_F16 : NOFFT_COEF_F32;
        o->flags |= NOFFT_OPT_COEF;
        return 0;
    }
    if (strcmp(key, "fit") == 0) {
        o->fit = (strcmp(val, "least-sq") == 0) ? NOFFT_FIT_LEAST_SQ
                                                : NOFFT_FIT_INTERP;
        o->flags |= NOFFT_OPT_FIT;
        return 0;
    }
    if (strcmp(key, "policy") == 0) {
        o->policy = (strcmp(val, "drift") == 0) ? NOFFT_POLICY_DRIFT
                  : (strcmp(val, "never") == 0) ? NOFFT_POLICY_NEVER
                                                : NOFFT_POLICY_ALWAYS;
        o->flags |= NOFFT_OPT_POLICY;
        return 0;
    }

    if (strcmp(key, "e") == 0 || strcmp(key, "err") == 0 || strcmp(key, "rate") == 0) {
        char* end = NULL;
        d = strtod(val, &end);
        if (end == val || *end != '\0')
            return -1;
        if (strcmp(key, "e") == 0) { o->e = d; o->flags |= NOFFT_OPT_E; }
        else if (strcmp(key, "err") == 0) { o->err = d; o->flags |= NOFFT_OPT_ERR; }
        else { o->rate = d; o->flags |= NOFFT_OPT_RATE; }
        return 0;
    }

    {
        char* end = NULL;
        l = strtol(val, &end, 10);
        if (end == val || *end != '\0' || l < 0)
            return -1;
    }
    if (strcmp(key, "bits") == 0) { o->bits = (uint16_t)l; o->flags |= NOFFT_OPT_BITS; }
    else if (strcmp(key, "block") == 0) { o->block_len = (uint32_t)l; o->flags |= NOFFT_OPT_BLOCK; }
    else if (strcmp(key, "step") == 0) { o->step = (uint32_t)l; o->flags |= NOFFT_OPT_STEP; }
    else if (strcmp(key, "baskets") == 0) { o->baskets = (uint32_t)l; o->flags |= NOFFT_OPT_BASKETS; }
    else if (strcmp(key, "degree") == 0) { o->degree = (uint16_t)l; o->flags |= NOFFT_OPT_DEGREE; }
    else if (strcmp(key, "frame") == 0) { o->frame_len = (uint32_t)l; o->flags |= NOFFT_OPT_FRAME_LEN; }
    else return -1;
    return 0;
}

static int parse_opts(nofft_codec_opts* o, int argc, char** argv, int start)
{
    for (int i = start; i < argc; i++) {
        char* eq = strchr(argv[i], '=');
        if (eq == NULL) {
            /* `auto` is the one flag with no value to give. */
            if (strcmp(argv[i], "auto") == 0) {
                o->flags |= NOFFT_OPT_AUTO;
                continue;
            }
            fprintf(stderr, "expected key=value, got '%s'\n", argv[i]);
            return -1;
        }
        *eq = '\0';
        if (set_opt(o, argv[i], eq + 1) != 0) {
            fprintf(stderr, "bad option '%s=%s'\n", argv[i], eq + 1);
            return -1;
        }
    }
    return 0;
}

static int cmd_list(void)
{
    int n = nofft_codec_autoload();
    for (int i = 0; i < n; i++) {
        const nofft_codec* c = nofft_codec_at((size_t)i);
        printf("%-6s  .%-7s  %s\n", c->name, c->extension, c->description);
    }
    return 0;
}

static int cmd_count(void)
{
    printf("%d\n", nofft_codec_autoload());
    return 0;
}

static int cmd_load(const char* path)
{
    if (nofft_codec_load(path) != 0) {
        fprintf(stderr, "failed to load module '%s'\n", path);
        return 1;
    }
    return 0;
}

static int cmd_dir(const char* dir)
{
    nofft_codec_load_dir(dir);
    return 0;
}

static int cmd_info(const char* name, const char* in_path)
{
    uint8_t* data = NULL;
    size_t n = 0;
    Err e = nofft_codec_read_file(in_path, &data, &n);
    if (e != ERR_OK) {
        fprintf(stderr, "cannot read '%s': %s\n", in_path, g_err_str(e));
        return 1;
    }
    nofft_codec_autoload();

    if (strcmp(name, "-") != 0) {
        const nofft_codec* c = nofft_codec_find(name);
        if (c == NULL) {
            fprintf(stderr, "no such codec '%s'\n", name);
            nofft_codec_free(data);
            return 1;
        }
        nofft_codec_info info;
        Err ie = (c->info != NULL) ? c->info(data, n, &info) : ERR_BAD_ARGS;
        if (ie != ERR_OK) {
            fprintf(stderr, "%s: not a %s container\n", in_path, name);
            nofft_codec_free(data);
            return 1;
        }
        printf("%-6s  rate %u  ch %u  frames %llu  payload %llu  bits %u  fmt %u\n",
               c->name, info.sample_rate, info.channels,
               (unsigned long long)info.frames, (unsigned long long)info.payload,
               info.bits, info.format);
    } else {
        int shown = 0;
        for (size_t i = 0; i < nofft_codec_count(); i++) {
            const nofft_codec* c = nofft_codec_at(i);
            nofft_codec_info info;
            if (c->info == NULL || c->info(data, n, &info) != ERR_OK)
                continue;
            printf("%-6s  rate %u  ch %u  frames %llu  payload %llu  bits %u  fmt %u\n",
                   c->name, info.sample_rate, info.channels,
                   (unsigned long long)info.frames, (unsigned long long)info.payload,
                   info.bits, info.format);
            shown++;
        }
        if (shown == 0)
            fprintf(stderr, "no codec claims '%s'\n", in_path);
    }
    nofft_codec_free(data);
    return 0;
}

static int cmd_encode(const char* name, const char* in_path, const char* out_path,
                      int argc, char** argv, int start)
{
    const nofft_codec* c = nofft_codec_find(name);
    if (c == NULL) {
        fprintf(stderr, "no such codec '%s'\n", name);
        return 1;
    }

    nofft_codec_opts opts;
    nofft_codec_opts_default(&opts);
    if (parse_opts(&opts, argc, argv, start) != 0)
        return 2;

    pcm_buf pcm;
    Err e = wav_load(in_path, &pcm);
    if (e != ERR_OK) {
        fprintf(stderr, "cannot load '%s': %s\n", in_path, g_err_str(e));
        return 1;
    }

    uint8_t* buf = NULL;
    size_t n = 0;
    e = c->encode(&pcm, &opts, &buf, &n);
    if (e != ERR_OK) {
        fprintf(stderr, "encode failed: %s\n", g_err_str(e));
        pcm_free(&pcm);
        return 1;
    }

    e = nofft_codec_write_file(out_path, buf, n);
    nofft_codec_free(buf);
    if (e != ERR_OK) {
        fprintf(stderr, "cannot write '%s': %s\n", out_path, g_err_str(e));
        pcm_free(&pcm);
        return 1;
    }

    printf("%s: %llu samples, %zu bytes, %.4f bits/sample\n",
           c->name, (unsigned long long)pcm.count, n,
           pcm.count ? (8.0 * (double)n / (double)pcm.count) : 0.0);
    pcm_free(&pcm);
    return 0;
}

static int cmd_decode(const char* name, const char* in_path, const char* out_path)
{
    const nofft_codec* c = nofft_codec_find(name);
    if (c == NULL) {
        fprintf(stderr, "no such codec '%s'\n", name);
        return 1;
    }

    pcm_buf pcm;
    Err e = nofft_codec_decode_file(c, in_path, &pcm);
    if (e != ERR_OK) {
        fprintf(stderr, "decode failed: %s\n", g_err_str(e));
        return 1;
    }
    e = wav_save(out_path, &pcm);
    if (e != ERR_OK) {
        fprintf(stderr, "cannot write '%s': %s\n", out_path, g_err_str(e));
        pcm_free(&pcm);
        return 1;
    }
    pcm_free(&pcm);
    return 0;
}

static int cmd_roundtrip(const char* name, const char* in_path,
                         int argc, char** argv, int start)
{
    const nofft_codec* c = nofft_codec_find(name);
    if (c == NULL) {
        fprintf(stderr, "no such codec '%s'\n", name);
        return 1;
    }

    nofft_codec_opts opts;
    nofft_codec_opts_default(&opts);
    if (parse_opts(&opts, argc, argv, start) != 0)
        return 2;

    pcm_buf pcm;
    Err e = wav_load(in_path, &pcm);
    if (e != ERR_OK) {
        fprintf(stderr, "cannot load '%s': %s\n", in_path, g_err_str(e));
        return 1;
    }

    uint8_t* buf = NULL;
    size_t n = 0;
    e = c->encode(&pcm, &opts, &buf, &n);
    if (e != ERR_OK) {
        fprintf(stderr, "encode failed: %s\n", g_err_str(e));
        pcm_free(&pcm);
        return 1;
    }

    pcm_buf dec;
    e = c->decode(buf, n, &dec);
    nofft_codec_free(buf);
    if (e != ERR_OK) {
        fprintf(stderr, "decode failed: %s\n", g_err_str(e));
        pcm_free(&pcm);
        return 1;
    }

    double snr = -1.0;
    if (dec.count == pcm.count)
        snr = compute_snr(pcm.samples, dec.samples, pcm.count);

    printf("%s: %llu samples, %zu bytes, %.4f bits/sample, SNR %.2f dB\n",
           c->name, (unsigned long long)pcm.count, n,
           pcm.count ? (8.0 * (double)n / (double)pcm.count) : 0.0, snr);

    pcm_free(&dec);
    pcm_free(&pcm);
    return 0;
}

int main(int argc, char** argv)
{
    if (argc < 2) {
        usage(argv[0]);
        return 2;
    }

    const char* cmd = argv[1];

    if (strcmp(cmd, "list") == 0)
        return cmd_list();
    if (strcmp(cmd, "count") == 0)
        return cmd_count();
    if (strcmp(cmd, "load") == 0 && argc == 3)
        return cmd_load(argv[2]);
    if (strcmp(cmd, "dir") == 0 && argc == 3)
        return cmd_dir(argv[2]);
    if (strcmp(cmd, "info") == 0 && argc == 4)
        return cmd_info(argv[2], argv[3]);
    if (strcmp(cmd, "encode") == 0 && argc >= 5)
        return cmd_encode(argv[2], argv[3], argv[4], argc, argv, 5);
    if (strcmp(cmd, "decode") == 0 && argc == 5)
        return cmd_decode(argv[2], argv[3], argv[4]);
    if (strcmp(cmd, "roundtrip") == 0 && argc >= 4)
        return cmd_roundtrip(argv[2], argv[3], argc, argv, 4);

    usage(argv[0]);
    return 2;
}
