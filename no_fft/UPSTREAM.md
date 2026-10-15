# Vendored: No_FFT codecs

This directory is a **verbatim, unmodified** copy of the codec sources from

    https://github.com/SarK0Y/No_FFT_v0.git

pinned to the `trunk` branch (dev mode).

| Field   | Value |
| ------- | ----- |
| Repo    | `https://github.com/SarK0Y/No_FFT_v0.git` |
| Mode    | `dev` |
| Branch  | `trunk` |
| Commit  | `7c20bf2bbad94395b5cfc27bc81a9bb048b32299` |
| Date    | `2026-10-15T12:50:14+03:00` |

## Vendored files

| File | Origin |
| ---- | ------ |
| `nofft.cpp`, `nofft.h` | polynomial-fit codec (`.no_fft`) |
| `acpcm.cpp`, `acpcm.h` | DPCM/basket codecs (`.nadc`, NFA1–NFA6) |
| `third_party/dr_wav.h` | upstream's WAV reader dependency |
| `LICENSE` | upstream licence |

Nothing here is edited by Nova: all glue lives in `crates/nova-codec`
(`shim/nova_nofft_shim.cpp` gives the C++ code a stable C ABI) and
`crates/nova-codec/build.rs`. Keeping the checkout pristine makes re-syncing
a clean copy with no merge.

## Refreshing

    scripts/sync-nofft.sh                 # dev: fetch branch trunk
    scripts/sync-nofft.sh --release       # release: fetch branch main
    scripts/sync-nofft.sh --branch <name> # explicit branch override

The script rewrites this file with the new commit, date and branch.
