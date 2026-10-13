# Vendored: No_FFT codecs

This directory is a **verbatim, unmodified** copy of the codec sources from

    https://github.com/SarK0Y/No_FFT_v0.git

pinned to the `main` branch (trunk).

| Field   | Value |
| ------- | ----- |
| Repo    | `https://github.com/SarK0Y/No_FFT_v0.git` |
| Branch  | `main` |
| Commit  | `47ca13f1c13d596052e06dd747d425f9784dfafe` |
| Date    | `2026-10-11T00:48:14+03:00` |

## Vendored files

| File | Origin |
| ---- | ------ |
| `nofft.cpp`, `nofft.h` | polynomial-fit codec (`.no_fft`) |
| `acpcm.cpp`, `acpcm.h` | DPCM/basket codecs (`.nadc`, NFA1–NFA4) |
| `third_party/dr_wav.h` | upstream's WAV reader dependency |
| `LICENSE` | upstream licence |

Nothing here is edited by Nova: all glue lives in `crates/nova-codec`
(`shim/nova_nofft_shim.cpp` gives the C++ code a stable C ABI) and
`crates/nova-codec/build.rs`. Keeping the checkout pristine makes re-syncing
from `main` a clean copy with no merge.

## Refreshing from main/trunk

    scripts/sync-nofft.sh                 # fetch branch main
    scripts/sync-nofft.sh --branch main   # explicit

The script rewrites this file with the new commit, date and branch.
