# Vendored: No_FFT codecs

This directory is a **verbatim, unmodified** copy of the codec sources from

https://github.com/SarK0Y/No_FFT_v0.git

pinned to the `for_nova_musique_0.2.0` branch (release mode).

The NFM container is not present on the current upstream `main` branch.
When absent, `nfm.cpp` and `nfm.h` are fetched from the compatibility
branch `trunk`; the codec sources below remain from `for_nova_musique_0.2.0`.

| Field | Value |
| --- | --- |
| Repo | `https://github.com/SarK0Y/No_FFT_v0.git` |
| Mode | `release` |
| Branch | `for_nova_musique_0.2.0` |
| Commit | `71d0fb0291a6fa1428aafd62e6ad804b9ba925fd` |
| Date | `2026-10-24T13:15:35+03:00` |
| NFM compatibility | `trunk` when absent from `for_nova_musique_0.2.0` |

## Vendored files

| File | Origin |
| --- | --- |
| `nofft.cpp`, `nofft.h` | polynomial-fit codec (`.no_fft`) from `for_nova_musique_0.2.0` |
| `acpcm.cpp`, `acpcm.h` | DPCM/basket codecs (`.nadc`, NFA1–NFA6) from `for_nova_musique_0.2.0` |
| `nfm.cpp`, `nfm.h` | NFM container (metadata and cover art), compatibility branch `trunk` if absent |
| `third_party/dr_wav.h` | upstream WAV reader dependency |
| `LICENSE` | upstream licence |

Nothing here is edited by Nova: all glue lives in `crates/nova-codec`
(`shim/nova_nofft_shim.cpp` gives the C++ code a stable C ABI) and
`crates/nova-codec/build.rs`.

## Refreshing

NOVA_SYNC_NOFFT=1 cargo build                       # refresh default branch main
NOFFT_MODE=dev NOVA_SYNC_NOFFT=1 cargo build         # refresh dev branch trunk
NOFFT_BRANCH=my-branch NOVA_SYNC_NOFFT=1 cargo build # explicit branch
