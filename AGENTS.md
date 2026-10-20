# Calculation-engine development

ISO 9613 calculation-engine development must be guided by **ISO/TR 17534-3:2015**
and the quality framework in ISO 17534-1. Before changing acoustic algorithms,
geometry preparation, numerical settings or validation claims, read the
[calculation definition](docs/ALGORITHM.md), the
[request/result contract](docs/API.md), the
[terrain conventions](docs/TERRAIN.md) and the verification notes in `README.md`.

- Keep the calculation target explicit: ISO 9613-2:2024 with ISO 9613-1:1993
  atmospheric absorption. Review the applicability of the 2015 report to that
  edition; do not silently mix 1996 and 2024 rules or reference results.
- Link acoustic changes to the relevant standard clauses, implementation,
  independent reference evidence and regression tests. Record unresolved
  interpretations and unsupported cases in the docs and capability document.
- Use verified published case inputs, intermediate results and acceptance
  intervals when available. Never invent ISO reference values or use production
  output as its own independent reference. Label adapted cases separately.
- Keep capability, validation and conformity claims proportional to the
  evidence. Missing, unsupported or unverified cases are not passes.

## Kernels

`src/kernels.rs` lets another program install faster versions of two inner
loops before the engine starts.

- Do all algorithm work in this repository. Do not add vector intrinsics,
  `target_feature` code or other CPU-specific speedups here.
- A kernel is a contract: the scalar version defines the result bit for bit.
  Changing a kernel's signature or meaning breaks every kernel set built
  against it, so treat it as an interface change.
- A speedup that changes any result is not a kernel; it is an algorithm change
  and needs its own evidence.

## Scope

engine-iso is only the calculation engine: it has no network interface and is
controlled by acalc-engine over standard input and output (the engine protocol,
`src/engine.rs`). HTTP routes, keys, jobs, farms and deployment belong to
acalc-engine; keep them out of this repository.

Keep this repository lean: only what is needed to build, run, test and ship the
engine. Keep studies, experiment output, benchmarks, notes, standards documents
and other non-public material out of it. Regression fixtures the tests read
belong in `tests/fixtures/`; keep their provenance to public identities
(document titles, catalogue URLs, hashes, clauses and tables), never local paths.

The 19 ISO/TR 17534-3 QGIS example projects with published reference
Measurements are maintained in acalc-engine (`examples/iso17534-3/`), built from
the fixtures and provenance in `tests/fixtures/iso17534/`. Changing those
fixtures changes the hashes acalc-engine pins, so update both together. Do not
add the licensed report or calculated results to either example distribution.

## Precompiled downloads

The Linux and Windows x86-64 distributions belong in `releases/<version>/`
(raw package version), indexed by `releases/README.md`; keep earlier versions.
The scripts in `scripts/` build and package those binaries; they are part of
shipping the engine. Keep the direct README links, checksums, source provenance
and license notices current when adding a release. Build baseline x86-64 without
host-specific CPU optimizations. Keep credentials and local build caches out of
archives. Report cross-compilation and Wine smoke checks separately from native
Windows testing.
