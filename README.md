# engine-iso — ISO 9613-2:2024 calculation engine

An independent Rust implementation of the [calculation definition](docs/ALGORITHM.md)
and [request/result contract](docs/API.md). It targets **ISO 9613-2:2024** with
**ISO 9613-1:1993** atmospheric absorption.

## Download and install

New to Acalc? Start with the **[QGIS plugin and complete setup guide](https://github.com/coloradoanalytics/acalc-qgis#install-and-run-acalc)**.
Download [acalc-qgis-0.8.3.zip](https://github.com/coloradoanalytics/acalc-qgis/raw/refs/heads/main/releases/0.8.3/acalc-qgis-0.8.3.zip), leave it zipped, and install it in QGIS
via **Plugins → Manage and Install Plugins → Install from ZIP**. Enable **Acalc**
and open **Plugins → Acalc → Acalc**. QGIS 3.34 or later in the 3.x series is required.

No Git, Rust or compiler is needed. Download both programs for your
**64-bit Intel or AMD computer** (x86-64). These are the current releases;
the engine-iso checksums and build details are in [releases](releases/README.md).

| Computer | Acalc Engine service | ISO calculation engine |
| --- | --- | --- |
| Linux x86-64 | [acalc-engine-0.1.2-linux-x86_64.tar.gz](https://github.com/coloradoanalytics/acalc-engine/raw/refs/heads/main/releases/0.1.2/acalc-engine-0.1.2-linux-x86_64.tar.gz) | [engine-iso-0.1.2-linux-x86_64.tar.gz](https://github.com/coloradoanalytics/engine-iso/raw/refs/heads/main/releases/0.1.2/engine-iso-0.1.2-linux-x86_64.tar.gz) |
| Windows x86-64 | [acalc-engine-0.1.2-windows-x86_64.zip](https://github.com/coloradoanalytics/acalc-engine/raw/refs/heads/main/releases/0.1.2/acalc-engine-0.1.2-windows-x86_64.zip) | [engine-iso-0.1.2-windows-x86_64.zip](https://github.com/coloradoanalytics/engine-iso/raw/refs/heads/main/releases/0.1.2/engine-iso-0.1.2-windows-x86_64.zip) |

1. Extract the service archive into a writable folder. It holds `acalc-engine`
   (Windows: `acalc-engine.exe`), `LICENSES.txt` and an empty `engines` folder.
2. Extract the engine archive into that `engines` folder, keeping the archive's
   own folder. The engine then is
   `engines/engine-iso-0.1.2-linux-x86_64/engine-iso-0.1.2` (Windows:
   `engines\engine-iso-0.1.2-windows-x86_64\engine-iso-0.1.2.exe`). On Linux,
   run `tar -xzf engine-iso-0.1.2-linux-x86_64.tar.gz -C engines`; on Windows,
   right-click the zip → **Extract All** and choose the `engines` folder.
3. Start **acalc-engine**, then open <http://127.0.0.1:8342>. The service finds
   the engine in `engines` at startup, from any working folder, and the
   **Installed engines** screen should show ISO ready. If several engine-iso
   versions are installed, choose one on the **Engines** page; use **Refresh**
   there after adding one while the service runs. In the QGIS Acalc
   plugin, set the service address to `http://127.0.0.1:8343` and leave the
   API key empty.

On Linux, run `./acalc-engine` (or its full path); the archives keep the
programs executable. The Linux engine needs the system's glibc 2.35 or later
(Ubuntu 22.04, Debian 12 or newer); no separate Rust runtime is required. The
Windows program is a command-line
engine started by the service; double-clicking `engine-iso-<version>.exe` is not the way
to run a calculation.

The links above download the actual archives. Saving a repository file-view
page does not download the binary. Each release folder's `SHA256SUMS` and
`BUILD-INFO.json` ([0.1.2](releases/0.1.2/)) identify its artifacts.
The Linux packages have been exercised locally; the 0.1.0 Windows package was
smoke-tested under Wine, with native Windows testing still outstanding.
For farms, use the same target and build on every node: identical source alone
does not guarantee bit-identical results across different math libraries.

> **Try it:** The [ISO/TR 17534-3 examples in acalc-engine](https://github.com/coloradoanalytics/acalc-engine/tree/main/examples/iso17534-3)
> contain all 19 test cases as Acalc QGIS projects, with the standard's published
> expected results stored as Measurements for manual comparison. The projects and
> their rebuild script are maintained in `acalc-engine`.

This is an engineering implementation, **not a verified ISO conformity release**.
Run `engine-iso --capabilities` for the executable combinations and limits, and
see [terrain preparation conventions](docs/TERRAIN.md) for interpretation identity.

engine-iso is only the calculation engine. It has **no network interface**:
acalc-engine, the Acalc processing node, starts it as a local program and
controls it over standard input and output with the engine protocol.
[acalc-engine](https://github.com/coloradoanalytics/acalc-engine) serves the local
calculation API to QGIS and manages engine configuration, jobs and results.

## Build from source

For developers: dependencies are pinned in `Cargo.lock`.
To build both release archives with Docker, run
[`scripts/build_binaries.sh`](scripts/build_binaries.sh). The build container
installs MinGW-w64 and the Windows Rust target; no host
cross-compiler installation is needed. The script writes
`releases/<package version>/` with the archives, `SHA256SUMS`, `BUILD-INFO.json`
(source hashes, Rust version, engine identity and
hashes) and an installation guide, and refreshes the
[releases index](releases/README.md). It leaves an identical existing release
unchanged and refuses to overwrite a differing one unless run with `--replace`;
other versions are never changed. After a release, update the download table
above to the new version.

To build only for the current machine:

```bash
cargo build --release --locked
```

The build produces one program, `target/release/engine-iso`. Distribute and
install it under its version, which `engine-iso --version` prints, in a folder
inside acalc-engine's `engines` folder:

```bash
version="$(target/release/engine-iso --version | cut -d' ' -f2)"
cp target/release/engine-iso "engine-iso-$version"
```

(on Windows, `engine-iso.exe` becomes `engine-iso-<version>.exe`). Put it in,
for example, `engines/engine-iso-<version>/` beside `acalc-engine`, which finds
it as its `iso` engine (use **Refresh** on its **Engines** page if it is running);
acalc-engine starts it when needed. The version in
the file name is a label: acalc-engine reports the version the engine itself
gives in its `hello` reply.

```text
engine-iso [--threads N] [--plan-cache N]   serve the engine protocol on stdin/stdout
engine-iso --describe                       print the engine's identity (version, build_revision, workers, kernels)
engine-iso --version                        print the engine's version
engine-iso --capabilities                   print the capability document
```

Omitting `--threads` uses all processor cores available to the process.
`--plan-cache` (default 4) is the number of prepared farm plans kept for
running work items.

CPU work uses one shared Rayon work-stealing pool. A calculation is divided
into work items of one source, one block of receivers and (with a pruning
strategy) one reflection order; each source-receiver pair lies in exactly one
item, and energy sums are exact (correctly rounded), so without pruning results
do not depend on scheduling, worker count or how the work is divided. With a
pruning strategy, live cutoffs depend on which items have completed, so pruned
results can differ slightly between runs.

engine-iso is a **divisible** engine: through the protocol's work-item
operations, acalc-engine can run one calculation's items on several machines
and merge them here. Unpruned farm results are bit-identical to a single
engine's when every node runs the same `build_revision` and math library.

## Kernels

Two inner loops, the segment broad-phase box test and the filtered orientation
test of the lateral search, go through `src/kernels.rs`. This build runs the
scalar kernels. Another program may install a different kernel set before
the engine starts; every kernel must decide exactly as its scalar version, so
kernels change speed, never results. An installed set's revision is part of
`build_revision`, and `--describe` names the kernels running.

## Joint contact solver

The production reflected mixed-path resolver is `src/joint_contact.rs`.
It solves coupled finite-edge contacts simultaneously through image geometry
and verified interval root covers. Necessary unfolded plan constraints narrow
finite-edge domains before root search, avoiding image-intersection poles in
that contraction step, with separate automatic support-sequence
discovery and full-route validation. The preceding alternating resolver is no
longer in the production path. Historical helper checks in
`src/legacy_contacts.rs` compile only for tests. General topology completeness and global minimum-path discovery remain
unverified.

## Acoustic and numerical behavior

- Point, line and triangle-mesh area sources retain their original identities.
  Explicit zones conserve actual length/area and supplied power. Numeric 0 dB
  is real power; a null octave means silence.
- Terrain uses a deterministic constrained triangulation with staged inference.
  Ground references, acoustic elevation and source height remain distinct.
  Receiver areas are row-major point grids and exclude included building footprints.
- Ground regions support both API overlap policies. General ground requires
  planar applicability; `mean_path_height` is an explicit A-weighted approximation
  requiring the API's applicability declarations for extended requests.
- Buildings, material faces, barriers, directivity and supported reflection/
  screening combinations are prepared independently of contribution cutoffs.
  The source launch direction governs host emission and directivity.
- Every family shares one screening-suppressed ground reference. The total
  contribution cap is applied before quadrature.
- Reflected mixed paths use one bounded parameter per finite physical support;
  all moving roof, corner and terrain anchors are solved together. Root-cover
  exclusion, unique roots and unresolved regions are distinct outcomes. The
  user-selected 1 cm control bounds contact enclosure width, not acoustic error.
  Finite-face, clearance, section-plane and reflection-direction checks remain
  separate. Limits are 16 supports, 256 discovered sequences per initial choice
  and 20,000 interval boxes per sequence. Unresolved work fails explicitly;
  general topology completeness and global minimum-path discovery are unverified.
- Long-term descriptors belong to physical sources and remain fixed throughout
  subdivision. Irregular-terrain and nonvertical-reflected long-term profiles
  remain unsupported.
- Search iterates sources and finite reflection beams, querying a 3D receiver
  index for connections. Back-facing surfaces and receivers outside a beam are
  rejected early. Order-one beams never query another reflector. Building floors
  do not exist as reflectors. Walls, planar roofs and roof facets are potential
  reflectors by default, subject to beam intersection and outward-facing geometry.
  Explicit `reflecting_faces` overrides still restrict the selected surfaces.
- Progress `path_counts` count calculations: each admitted receiver of a source
  stub or reflection beam (including each area-grid point) spawns one terminal
  calculation stub, `checked` counts them and `pruned` counts those that kept no
  energy and have omitted energy. Octave bands and quadrature samples are not
  counted separately. The counts equal `diagnostics.path_search.receiver_stub_counts`
  and do not depend on how the work is divided. Beams, empty beams and
  reflector candidates are separate `path_search` diagnostics. An empty beam
  contains no receiver (before exact geometry tests).
- Pruning is off by default. `"pruning_strategy": "dba"` or `"dbc"` runs each
  reflection order of each source as a task, without waiting between orders.
  Once all direct paths are done, each task takes per-band cutoffs when it
  starts from the quietest receiver's weighted level so far, less
  10 log10(source count) + 15 dB (dBA) or + 20 dB (dBC). Pruned results can
  differ slightly between runs. A fixed `"minimum_contribution_db"` is also accepted (not with a
  strategy). Either prunes only on conservative bounds before path
  construction: reflection prefixes (distance to the reflector plus to the
  nearest receiver, and the reflector size criterion), the unscreened family cap
  for point families with vertical reflectors, and summed cell bounds for
  line/area families. Computed contributions are never discarded. The retired
  `receiver_margin_db` input is accepted but ignored for request compatibility.
- Integration uses positive, power-conserving quadrature and explicit coverage
  diagnostics. Depth/precision exhaustion fails the job rather than returning
  a truncated field. A coarse/fine discrepancy is not a certified total prediction
  error. The numerical cutoff report is separate from integration uncertainty.

The reflected-screening, bent-reference mean-height and contribution-cap rules
include the documented engine interpretations. The special long-term industrial
screening treatment in ISO 9613-2:2024 §7.4.1 Note 1 is excluded. No industrial-site,
housing or detailed-forest profile or inverse fitting API is invented here.
Clear nonvertical reflections are separate from screened nonvertical geometry,
which currently returns an unsupported diagnostic. Foliage follows the specified
5 km arc convention and its declared geometry limits.

## Verification

```bash
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
```

Tests distinguish independently published coefficients, supplementary analytic
checks, geometry invariants, deterministic concurrency and engine-protocol
behavior (including farm items merged in any order). No production output is
used as its own independent ISO reference. All 19 ISO/TR 17534-3:2015 project
scenes are checked against separately derived 2024 analytical adaptations,
including retained geometry and intermediate terms
(`tests/fixtures/iso17534/provenance.json`). These are supplementary
references, not ISO-issued 2024 acceptance values, and passing them is not
ISO conformity.

## Contributions

Contributions are accepted under **GPL-3.0-only**, the same license as this
project. See [CONTRIBUTING.md](CONTRIBUTING.md) for the contribution terms.

## License

Copyright (C) 2026 Colorado Analytics. engine-iso is free software under the
GNU General Public License version 3 only; see [COPYRIGHT](COPYRIGHT) and
[LICENSE](LICENSE).
