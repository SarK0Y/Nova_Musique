# Vendored: No_FFT codecs

This directory is a **verbatim, unmodified** copy of the codec sources from

    https://github.com/SarK0Y/No_FFT_v0.git

pinned to the `trunk` branch (dev mode).

| Field   | Value |
| ------- | ----- |
| Repo    | `https://github.com/SarK0Y/No_FFT_v0.git` |
| Mode    | `dev` |
| Branch  | `trunk` |
| Commit  | `166c1bdcb3b950d45b7048c6adfe2819c024ea67` |
| Date    | `2026-10-21T20:45:56+03:00` |

## Vendored files

| File | Origin |
| ---- | ------ |
| `nofft.cpp`, `nofft.h` | polynomial-fit codec (`.no_fft`) |
| `acpcm.cpp`, `acpcm.h` | DPCM/basket codecs (`.nadc`, NFA1–NFA6) |
| `nfm.cpp`, `nfm.h` | NFM container (wraps any codec + metadata + cover) |
| `third_party/dr_wav.h` | upstream's WAV reader dependency (under no_fft/third_party/) |
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
