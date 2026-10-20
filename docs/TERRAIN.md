# Geometry preparation and evidence

The calculation target is ISO 9613-2:2024 with ISO 9613-1:1993 absorption.
Terrain construction is an application convention from `docs/API.md` and
`docs/ALGORITHM.md`; it is not an interpolation method prescribed by ISO.
The resulting profiles feed ground attenuation (7.3), screening (7.4), and the
terrain review recommended by ISO/TR 17534-3:2015, 5.8. The 2015 report's
1996-based scene results have not been adopted as 2024 acceptance values.

## Immutable construction identity

`iso-rust-cdt-spade-2.15.1-lexicographic-v1` pins these decisions:

- Spade 2.15.1 constrained Delaunay triangulation, with robust 1.2.0 orientation
  predicates. Dependencies are pinned in Cargo.toml and Cargo.lock. This is a
  Rust-specific revision; it does not assert equivalence to another backend.
- Ground observations are sorted lexicographically by X, Y, then Z before
  incremental insertion. Negative zero is normalized. Equal XY observations
  require Z agreement within 1e-7 m; the smallest consistent elevation is
  retained. Distinct authored XY observations are never averaged or merged.
- Breakline endpoints and segments are canonicalized and sorted before
  insertion. Existing observations lying on constraints must agree in elevation.
  Consistent crossings are split. Calculated crossing nodes within 1e-9 m of
  an existing observation/node reuse the lexicographically first such node;
  this prevents separate floating-point evaluations of the same multi-line
  crossing from creating microscopic artificial edges. Distinct original
  observations retain their coordinates. Inconsistent or unrepresentable
  constraints fail instead of disappearing.
- Triangle winding follows Spade's counterclockwise inner faces. Each triple
  is cyclically rotated to put its smallest index first, then all triples are
  sorted. Vertex indices refer to the sorted observations; constraint edges
  are sorted index pairs. Cocircular tie handling is pinned by the exact
  dependency and insertion sequence.
- Barycentric interpolation applies inside each triangle; a point exactly on an
  edge interpolates between the edge's endpoints taken in vertex-index order.
  Point location starts from a per-thread hint (the vertex found by that
  thread's previous lookup) with a static location hierarchy for insertion; the
  hint affects only search time, never the located triangle. Outside the convex
  hull, elevation is interpolated at the nearest hull boundary point. One
  observation defines a constant surface. Collinear observations interpolate
  piecewise along their common XY axis, extend transversely, and clamp beyond
  the two ends. No observations and no inference use the legacy ground plane.
- Every inference target samples the same immutable initial explicit surface.
  All inferred targets and all resolved breakline constraints then participate
  in the final surface. Acoustic Z, reflector bottoms, receiver-area boundaries,
  generated receivers, and attachment positions do not seed either surface.

Spade's [triangulation API](https://docs.rs/spade/2.15.1/spade/trait.Triangulation.html)
and [constraint API](https://docs.rs/spade/2.15.1/spade/struct.ConstrainedDelaunayTriangulation.html)
provide insertion, topology and geometric lookup. Implementation and tests are
in `src/geometry.rs`; this implementation does not copy the Go engine.

## Profiles, domains and resource behavior

Terrain profiles contain every crossed triangulation edge/vertex and the
exterior projection-transition positions. Each interval is affine, so signed
reference-minus-ground trapezoid integration is exact for that prepared surface.
Nearest-hull extension can bend a profile even when all terrain observations
lie on one plane: the ground method must also check the actual reference profile.
Terrain topology is immutable and shared by concurrent receiver operations.

Source surfaces use their supplied mesh connectivity. Planar polygon helpers
use first-valid-ear triangulation without rotating/reversing authored indices.
Roof connectivity is checked for nonoverlap, area coverage and containment in
the complete concave footprint. Geometry preparation checks emitting boundaries
and terrain vertices inside projected emitting triangles, so a terrain peak
cannot be hidden by clear triangle corners. Solid/mesh checks include both
directions of noncoplanar intersection. These tests concern input geometry;
they do not certify propagation integration over unresolved obstacle transitions.

Receiver grids use the contract's 1e-8 m inclusion tolerance and admit at most
one extra upper-axis endpoint per axis. Stable IDs retain computed float64
coordinates and original row/column order. Building footprint exclusions apply
at every receiver height. Source-height constancy considers complete terrain
profiles and interior overlay vertices, not only authored emitting vertices.

Terrain preparation checks cancellation on each work step and allows at most
250,000 ground vertices. There is no geometry-operation limit. The geometry helper accepts coordinates only
within ±1e12 m; the public request validator enforces the API's stricter ±1e9 m
coordinate profile. Polygon boundaries are limited to 10,000 vertices; barriers
to 4,096 base vertices. Resource failures do not silently reduce geometry.

## Verification status

Independent analytic fixtures check planes, nearest-hull projection, constant
and collinear surfaces, a ridge's exact 50 m² integrated ground profile,
exterior clamping, immutable staged inference, consistent and inconsistent
crossings, overlapping constraints, and a three-line rational intersection.
Order reversal checks deterministic output but is a metamorphic regression,
not an independent ISO reference case.

Scene fixtures check independently enumerated receiver grids and IDs, a 4 m²
attached emitter's power, rotated attachment placement, interior terrain/solid
penetration, mesh intersection symmetry, and varying height between equal-height
source endpoints. These are application geometry and equation-level evidence.
Published terrain cases, cross-backend identical-mesh fixtures and full ISO 17534
conformity remain unverified. No geometry test is reported as an official ISO
scene pass.
