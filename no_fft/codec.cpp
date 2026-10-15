/* codec.cpp - the codec registry and the loadable module loader.
 *
 * The registry is deliberately tiny: a vector of pointers to codec vtables.
 * Builtins are always available through nofft_codec_builtin(); registering them
 * just puts them in front of the same lookup a loaded module lands in, so an
 * application never has to know which codecs came from where.
 *
 * Loading is split so the same registry works whether codecs are linked in or
 * discovered at run time.  A module is a shared object exporting
 * nofft_codec_plugin; the loader calls it, checks the ABI stamp and registers
 * the result.  Handles are kept open for the life of the process because the
 * registry holds pointers into the module's memory.
 */

#include "codec.h"
#include "acpcm.h"

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

#if defined(_WIN32)
#  include <windows.h>
#  define NOFFT_MODULE_SUFFIX ".dll"
#  define NOFFT_PATH_SEP     ';'
#else
#  include <dlfcn.h>
#  include <dirent.h>
#  define NOFFT_MODULE_SUFFIX ".so"
#  define NOFFT_PATH_SEP     ':'
#endif

namespace {

#if defined(_WIN32)
typedef HMODULE module_t;
module_t module_open(const char* path) { return LoadLibraryA(path); }
void*    module_sym(module_t h, const char* sym)
{
    return (void*)GetProcAddress(h, (LPCSTR)sym);
}
void     module_close(module_t h) { FreeLibrary(h); }
#else
typedef void* module_t;
module_t module_open(const char* path) { return dlopen(path, RTLD_NOW | RTLD_LOCAL); }
void*    module_sym(module_t h, const char* sym) { return dlsym(h, sym); }
void     module_close(module_t h) { dlclose(h); }
#endif

std::vector<const nofft_codec*>& registry()
{
    static std::vector<const nofft_codec*> r;
    return r;
}

/* Handles of every module we successfully registered into, kept only so the
   process does not unload code the registry is still pointing at. */
std::vector<module_t>& module_handles()
{
    static std::vector<module_t> h;
    return h;
}

std::vector<std::string>& search_dirs()
{
    static std::vector<std::string> d;
    return d;
}

const nofft_codec* find_registered(const char* name)
{
    if (name == NULL)
        return NULL;
    for (size_t i = 0; i < registry().size(); i++)
        if (std::strcmp(registry()[i]->name, name) == 0)
            return registry()[i];
    return NULL;
}

int load_module(const std::string& path)
{
    module_t h = module_open(path.c_str());
    if (h == NULL)
        return -1;

    nofft_codec_plugin_fn fn =
        (nofft_codec_plugin_fn)module_sym(h, "nofft_codec_plugin");
    if (fn == NULL) {
        module_close(h);
        return -1;
    }

    const nofft_codec* c = NULL;
    if (fn(&c) != 0 || c == NULL || c->abi != NOFFT_CODEC_ABI) {
        module_close(h);
        return -1;
    }

    /* A module for a codec that is already registered (commonly the builtin) is
       not an error: the module loaded and advertised a codec, there is just
       nothing left to add.  The handle can be closed in that case because the
       registry keeps the builtin's vtable, not this one. */
    int r = nofft_codec_register(c);
    if (r < 0) {
        module_close(h);
        return -1;
    }
    if (r > 0) {
        module_close(h);
        return 0;
    }

    module_handles().push_back(h);
    return 0;
}

} /* namespace */

extern "C" {

void nofft_codec_opts_default(nofft_codec_opts* o)
{
    if (o == NULL)
        return;
    std::memset(o, 0, sizeof(*o));
    o->bits = 6;
    o->degree = 3;
    o->coef_format = NOFFT_COEF_F32;
    o->fit = NOFFT_FIT_INTERP;
    o->block_len = 1024;
    o->policy = ACP2_DRIFT;
    o->step = 5;
    o->baskets = 170;
    o->frame_len = 20;
    o->e = ACPCM5_E_DEFAULT;
    o->err = ACPCM5_ERR_DEFAULT;
    o->rate = ACPCM6_RATE_DEFAULT;
}

int nofft_codec_register(const nofft_codec* codec)
{
    if (codec == NULL || codec->name == NULL || codec->name[0] == '\0' ||
        codec->encode == NULL || codec->decode == NULL)
        return -1;
    if (codec->abi != NOFFT_CODEC_ABI)
        return -1;
    if (find_registered(codec->name) != NULL)
        return 1;
    registry().push_back(codec);
    return 0;
}

size_t nofft_codec_count(void)
{
    return registry().size();
}

const nofft_codec* nofft_codec_at(size_t index)
{
    if (index >= registry().size())
        return NULL;
    return registry()[index];
}

const nofft_codec* nofft_codec_find(const char* name)
{
    const nofft_codec* c = find_registered(name);
    if (c != NULL)
        return c;
    return nofft_codec_builtin(name);
}

void nofft_codec_free(void* p)
{
    free(p);
}

int nofft_codec_load(const char* path)
{
    if (path == NULL || path[0] == '\0')
        return -1;
    return load_module(path);
}

void nofft_codec_set_search_path(const char* path)
{
    search_dirs().clear();
    if (path == NULL)
        return;

    std::string cur;
    for (const char* p = path;; p++) {
        if (*p == NOFFT_PATH_SEP || *p == '\0') {
            if (!cur.empty())
                search_dirs().push_back(cur);
            cur.clear();
            if (*p == '\0')
                break;
        } else {
            cur += *p;
        }
    }
}

int nofft_codec_load_dir(const char* dir)
{
    if (dir == NULL || dir[0] == '\0')
        return 0;

    const size_t suffix = std::strlen(NOFFT_MODULE_SUFFIX);
    int loaded = 0;

#if defined(_WIN32)
    std::string pattern = std::string(dir) + "\\*" + NOFFT_MODULE_SUFFIX;
    WIN32_FIND_DATAA fd;
    HANDLE h = FindFirstFileA(pattern.c_str(), &fd);
    if (h == INVALID_HANDLE_VALUE)
        return 0;
    do {
        std::string p = std::string(dir) + "\\" + fd.cFileName;
        if (load_module(p) == 0)
            loaded++;
    } while (FindNextFileA(h, &fd));
    FindClose(h);
#else
    DIR* d = opendir(dir);
    if (d == NULL)
        return 0;
    struct dirent* e;
    while ((e = readdir(d)) != NULL) {
        const char* nm = e->d_name;
        size_t l = std::strlen(nm);
        if (l <= suffix || std::strcmp(nm + l - suffix, NOFFT_MODULE_SUFFIX) != 0)
            continue;
        std::string p = std::string(dir) + "/" + nm;
        if (load_module(p) == 0)
            loaded++;
    }
    closedir(d);
#endif
    return loaded;
}

int nofft_codec_autoload(void)
{
    /* A host that ships only modules can stay small: NOFFT_NO_BUILTINS skips
       the codecs that were linked in, leaving the registry to the search path.
       Anything other than a leading '0' counts as set. */
    const char* no_builtins = getenv("NOFFT_NO_BUILTINS");
    int want_builtins = 1;
    if (no_builtins != NULL && no_builtins[0] != '\0' && no_builtins[0] != '0')
        want_builtins = 0;

    if (want_builtins) {
        for (size_t i = 0; i < nofft_codec_builtin_count(); i++)
            nofft_codec_register(nofft_codec_builtin_at(i));
    }

    if (search_dirs().empty()) {
        const char* env = getenv("NOFFT_MODULE_PATH");
        if (env != NULL)
            nofft_codec_set_search_path(env);
    }
    for (size_t i = 0; i < search_dirs().size(); i++)
        nofft_codec_load_dir(search_dirs()[i].c_str());

    return (int)nofft_codec_count();
}

Err nofft_codec_read_file(const char* path, uint8_t** out, size_t* out_len)
{
    if (path == NULL || out == NULL || out_len == NULL)
        return ERR_BAD_ARGS;
    *out = NULL;
    *out_len = 0;

    FILE* f = fopen(path, "rb");
    if (f == NULL)
        return ERR_OPEN;

    std::vector<uint8_t> mem;
    uint8_t buf[65536];
    size_t got;
    while ((got = fread(buf, 1, sizeof(buf), f)) > 0)
        mem.insert(mem.end(), buf, buf + got);
    int ioerr = ferror(f);
    fclose(f);
    if (ioerr)
        return ERR_IO;

    uint8_t* m = (uint8_t*)malloc(mem.empty() ? 1u : mem.size());
    if (m == NULL)
        return ERR_NO_MEMORY;
    if (!mem.empty())
        std::memcpy(m, mem.data(), mem.size());
    *out = m;
    *out_len = mem.size();
    return ERR_OK;
}

Err nofft_codec_write_file(const char* path, const uint8_t* data, size_t len)
{
    if (path == NULL || (data == NULL && len != 0))
        return ERR_BAD_ARGS;

    FILE* f = fopen(path, "wb");
    if (f == NULL)
        return ERR_OPEN;

    Err rc = ERR_OK;
    if (len != 0 && fwrite(data, 1, len, f) != len)
        rc = ERR_IO;
    if (fclose(f) != 0 && rc == ERR_OK)
        rc = ERR_IO;
    return rc;
}

Err nofft_codec_encode_file(const nofft_codec* codec, const pcm_buf* in,
                            const nofft_codec_opts* opts, const char* out_path)
{
    if (codec == NULL || out_path == NULL)
        return ERR_BAD_ARGS;

    uint8_t* buf = NULL;
    size_t n = 0;
    Err e = codec->encode(in, opts, &buf, &n);
    if (e != ERR_OK)
        return e;

    e = nofft_codec_write_file(out_path, buf, n);
    nofft_codec_free(buf);
    return e;
}

Err nofft_codec_decode_file(const nofft_codec* codec, const char* in_path,
                            pcm_buf* out)
{
    if (codec == NULL || in_path == NULL || out == NULL)
        return ERR_BAD_ARGS;

    uint8_t* buf = NULL;
    size_t n = 0;
    Err e = nofft_codec_read_file(in_path, &buf, &n);
    if (e != ERR_OK)
        return e;

    e = codec->decode(buf, n, out);
    nofft_codec_free(buf);
    return e;
}

} /* extern "C" */
