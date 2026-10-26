# Calculation algorithm

This document describes how engine-iso calculates outdoor sound levels. It
follows **ISO 9613-2:2024** (propagation) with **ISO 9613-1:1993** (atmospheric
absorption), and covers incoherent point, line and area sources, point and
gridded receivers, terrain, ground regions, buildings, barriers and finite
planar reflectors. [API.md](API.md) defines the request and result format and
the numerical defaults.

The standard's equations and eligibility rules apply throughout. Where the
standard is silent or a rule needs a concrete construction, the engine uses an
**engine interpretation**; these are marked where they occur and listed in
[the last section](#engine-interpretations-and-exclusions).

## Overview

For each calculation the engine:

1. **Prepares the scene** once: validates inputs, builds the terrain surface,
   source domains, reflecting faces, atmospheric coefficients and spatial
   indexes, and generates receiver grids.
2. **Finds propagation families** for every source: the direct family and each
   ordered sequence of reflecting faces up to the chosen reflection order.
3. **Resolves the routes** of each family to each receiver: specular contacts,
   diffraction over and around obstacles, and every eligibility check.
4. **Calculates each route's level** from the ISO 9613-2 attenuation terms,
   caps each family at its unscreened level, and integrates extended sources
   over their area or length.
5. **Sums energies** per receiver, with exact arithmetic, and reports levels,
   per-source contributions and any omitted energy.

Energy here means incoherent relative mean-square pressure. All sums are of
energies, never of decibels. The eight octave bands are 63, 125, 250, 500,
1,000, 2,000, 4,000 and 8,000 Hz.

## Inputs and settings

Coordinates are Cartesian metres in one vertical datum. Acoustic positions (where
sound is emitted or received) are kept separate from ground elevations.

A calculation needs source geometry and sound power, receiver geometry,
terrain, ground factors, weather, reflection materials and method settings,
plus directivity and additional attenuation regions where present. An absent
optional effect contributes nothing; a present but unsupported effect is
reported as a diagnostic, never silently ignored.

Sound power levels are in dB re 1 pW. A numeric 0 dB is real power, not
silence; silence is stated explicitly. A positive quantity that overflows the
numerical range is an error, not a zero.

| Setting | Meaning |
| --- | --- |
| `ground_method` | `general` (default) or `mean_path_height`. One method applies to the whole calculation; terrain shape never switches it. |
| Meteorology | `downwind`, or `long_term` with `C_0` and the source descriptors described under *Long-term meteorological correction*. |
| Ground | A default ground factor `G` with ground-region overrides. Ground factor is separate from wall absorption. |
| Reflection material | Energy absorption per band in [0, 1]; default 0.1. Full absorption reflects nothing. |
| `max_reflect_order` | 0–5, counting reflections only. Order 0 still includes direct and screened paths. |
| `pruning_strategy` | `"dba"` or `"dbc"` enables automatic pruning protecting that weighted level; absent or `null` (default) disables it. |
| `minimum_contribution_db` | A fixed absolute band cutoff; absent or `null` (default) disables it. Cannot be combined with a strategy. |
| Area integration | Extent/distance factor, optional maximum extent, absolute and relative energy tolerances, and depth limits. These control refinement, not which paths exist. |

Without pruning, every eligible path up to the chosen reflection order is
evaluated. There is no separate limit on the number of diffractions and no
angular cutoff. Every cutoff comparison keeps equality (a value equal to the
cutoff is kept). Numerical and resource limits make unfinished work fail; they
never quietly shrink the set of paths.

## Scene preparation

The scene and resolved settings are frozen before calculation. Identities,
coordinates, geometry, units, source powers and method combinations are
validated, then source domains, meteorological descriptors, terrain, surface
planes and materials, atmospheric coefficients and spatial indexes are
prepared once and shared by all work.

### Terrain

The terrain surface is a triangulated mesh built from every explicit ground
elevation in the scene (terrain points and breaklines, and the ground
elevations of buildings, receivers and other objects) with their acoustic
elevations kept separately. Where an input gives ground as Z and height as M,
Z + M is an acoustic position, not a terrain sample. Missing ground elevations
are inferred from the explicit data; a placeholder never seeds the surface it
is inferred from. [TERRAIN.md](TERRAIN.md) gives the exact construction.

Footprint containment is a horizontal test that does not depend on the query's
elevation. Boundaries use a distance in metres (1e-8 m), not a cross-product
threshold whose effective tolerance would change with edge length. Generated
ground-anchored wall vertices within that distance may merge; profile endpoints
and the terrain mesh are unchanged.

### Receiver grids

An area receiver is a set of sample points, not one microphone. It is a simple
XY polygon (concave allowed, either winding, optional closing vertex) with a
height above terrain and a grid spacing; defaults are 1.5 m and 10 m.
Self-intersecting polygons, holes, rotated grids and custom origins are
rejected.

The grid starts at the polygon's minimum X and Y and runs in rows, then
columns. Points on the boundary are kept; points inside or on exclusion
polygons or included building footprints are removed, whatever the receiver
height. Each remaining point takes its ground elevation from the terrain, plus
the receiver height. Grids never change the terrain.

Generated points follow the explicit receivers, with stable parent, point, row
and column identities (API.md). An empty grid is valid: the engine never adds a
fallback point, widens the spacing or trims coverage to meet a limit. Each
sample is then an ordinary point receiver.

## Sources

### Power and emission zones

A point source carries its total band power. A line or area source consists of
uniform emission zones that keep the original source's identity. Line zones
use their actual length; area zones use their actual 3D area, including sloped
and vertical surfaces. Power densities are converted to zone totals once, and
an object's total is divided among its zones by measure unless zone totals are
given.

Area surfaces are triangulated without overlap, keeping concave boundaries and
holes; a domain that cannot be triangulated is rejected. A nonplanar surface
not attached to a building needs an explicit mesh. Lines are integrated along
their length, not replaced by unrelated point sources.

For a zone of measure M and an integration cell C:

```text
W_zone[b] = 10^(L_W_zone[b] / 10)
W_cell[b] = W_zone[b] * measure(C) / M
sum(child.W_cell[b]) = parent.W_cell[b]
E_cell[b] = integral over C of density[b] * T(x, receiver, family, b) dM
          ≈ W_cell[b] * sum_j(q_j * T(x_j, receiver, family, b)),   q_j > 0, sum_j(q_j) = 1
```

T is the energy received per unit emitted power. Each reflection family
integrates over the whole emitting domain; where geometry makes a family
impossible, T is simply zero there. Power is never redistributed from
ineligible parts or divided among families. Family energies add incoherently.

### Building-attached sources

A source on a building face represents sound radiating from inside the building
to the outside. Its emission data already describe that outward radiation, so
the engine applies the supplied power and directivity unchanged: it adds no
3 dB solid-angle correction and no reflection from the host face it sits on
(engine interpretation). Attached area sources must be planar; a source
spanning different face planes must be split. The outward normal comes from
the building solid, not from the source polygon's winding.

**Launch test.** For every source lying on a building face (point sources and
each line/area integration position), the first nonzero segment v of each
completed route is compared with the host face's outward normal n:

```text
dot(n, v) < 0   → the route carries no energy (it would launch into the building)
dot(n, v) >= 0  → the route is kept (outward or along the face)
```

This test uses the actual launch direction, not the receiver's bearing. Kept
directions get the declared power and directivity with no extra gain or taper.
Routes may start along the face; later segments must clear building interiors
and other obstacles. No artificial offset or zero-length host reflection is
added.

Attachment metadata names the host. An untagged source on a face needs one
unambiguous supporting face (coplanar seams count as one); a source touching
faces of different planes is reported as ambiguous.

**Point mountings.** A building-mounted point declares what its emission data
mean:

- `surface`: an opening, louvre or panel on the face. It lies on the face and
  follows the rules above: outward launch only, no host reflection.
- `standoff`: a unit mounted clear of the face whose data describe a free
  source. It is an ordinary point at least 0.05 m out along the outward normal,
  with no launch restriction, and the host face reflects it like any other.

The two differ by the host's image source, roughly (1 − α) of the direct
energy (up to about 3 dB). So that a millimetre of digitizing error cannot
choose between them, any point source from 1e-7 m to 0.05 m from a building
face it projects onto is rejected, attached or not. The 0.05 m clearance is an
engineering choice, not an ISO 9613-2 value.

## Finding propagation paths

### Reflecting faces

Every exterior building face (walls, planar roofs and individual roof facets)
is a potential reflector unless the request selects faces explicitly. Floors
are not reflectors. Roof normals point outward and upward. A face becomes a
reflection candidate only if it intersects the incoming beam and faces the
source or image it is lit from.

### Families and beams

A **family** is one ordered sequence of reflecting faces; the empty sequence is
direct propagation. Families are found by tracing **beams** from each source:

- A beam from a source (or its image) is clipped by each reflecting face's
  finite window and continues as the image beam behind it, up to the chosen
  order. A beam at the final order only looks for receivers.
- Each beam queries a 3D receiver index. Every receiver the beam admits
  (including each area-grid point) becomes its own **receiver stub** with its
  own reference geometry and band mask; stubs share only the immutable
  reflection history and never extend the beam tree.
- Faces behind the emitting domain are rejected by half-space tests. Extended
  source envelopes cover every cell vertex, not only a centroid. Beams are
  conservative: they may include extra candidates but never miss an eligible
  launch point. Screening obstacles do not clip beams, because a screened route
  can still carry energy.

Diffraction is evaluated on these connections; diffraction edges never become
sources, search states or new beams.

### Final contacts and eligibility

Beam positions only guide the search. For each actual receiver and source
sample, every reflection contact is solved again together with the diffraction
around it, and the route must pass all of:

- the specular reflection geometry and reflecting side;
- the face's finite bounds;
- clearance of every leg from obstacles;
- the per-band reflector size test, ISO 9613-2:2024 equation (26), using the
  final incoming and outgoing geometry.

For h in equation (27) the engine uses the exposed vertical extent of the face
at the reflection point (including sloping upper and lower edges and, for
ground-anchored walls, the local terrain), not the face's global extremes.

A route that misses a required face is infeasible: it is not clamped to an
edge or counted with a fictitious loss. Proven infeasibility rejects only that
candidate; other receivers, routes and higher-order families continue.
Numerical failure is never treated as proof of infeasibility.

### Lateral routes

Lateral (sideways) diffraction routes run around obstacles in a section plane.
The search:

- considers buildings and faces near each leg, using spatial indexes that only
  exclude objects whose bounds cannot touch the leg;
- ignores buildings wholly behind a leg's start or beyond its end;
- for a concave building, keeps corners behind the source or beyond the
  receiver when the route must escape a courtyard (as in ISO/TR 17534-3:2015
  T18, Table 60), while the side envelope between them stays convex;
- decides which side a route is on along the source–receiver segment, not by
  its first escape corner;
- includes the roof edge and facet crossings where the lateral plane leaves a
  building through its roof (ISO/TR 17534-3 T12/T15, Figures 20/26).

Obstacle clearance is checked on every finite leg. Contacts with walls and
roofs use the 1e-8 m boundary distance, so roundoff at a leg endpoint is not an
obstruction. Buildings use their footprint width; barriers use their whole
width, in authored vertex order.

## Screening

### Diffraction per route

Each route is screened by its whole obstacle chain and terrain profile; losses
of individual obstacles are never added independently.

| Completed route | How diffraction is calculated |
| --- | --- |
| No reflection | One calculation per eligible route through the uninterrupted obstacle chain. |
| Reflections, vertical diffraction only | Unfold the whole family and its profiles, calculate one combined vertical loss, then refold and verify every contact. This follows the [DAGA vertical-screening proposal][mixedscreening]. |
| Reflections with any lateral diffraction | Split the route at its reflections, calculate each section, and add the section losses. Each section may go over or around (engine interpretation). |

A mixed route does not also receive the all-vertical whole-route loss.
Extending a family recalculates its combined profile rather than adding a
previous prefix's loss.

Within each diffraction calculation the ISO 7.4 eligibility rules, signed path
differences, meteorological factors and single/multiple-edge limits apply. No
extra 20/25 dB cap is applied to the sum of separate sections. Terrain that
supports the top path excludes lateral diffraction; lateral paths must
otherwise clear the terrain. Distinct complete routes add in energy, and routes
are deduplicated so a route is never counted twice.

A completed route that pins a vertical-wall reflection to a diffraction edge is
rejected when the required kink at that edge violates the mirror direction;
alternative routes are kept. Other angle failures remain numerical errors.

### Solving reflected screening

When a route reflects from vertical mirrors and also diffracts, the
reflection contacts and diffraction points depend on each other. The engine
solves them together.

**Supports.** The scene's possible diffraction edges are catalogued once:
vertical building corners, roof and facet edges, barrier and panel edges, and
terrain mesh edges. Each diffraction point on a route ("anchor") has one
parameter along its finite edge. For a given sequence of edges, the mirror
contacts are eliminated by image construction between neighbouring anchors,
and all anchor parameters are solved at once with their section roles fixed.

**Equations.** For a section from A to B, with D = B − A and anchor V = P − A:

```text
lateral section:   Vz*(Dx² + Dy²) − Dz*(Vx*Dx + Vy*Dy) = 0
vertical section:  Dx*Vy − Dy*Vx = 0
```

These keep the usual section planes; the acoustic attenuation formulas do not
change.

**Contacts.** Each mirror contact is built directly between its two anchors:
the earlier anchor imaged forward through the preceding mirrors, and the later
anchor mapped back through this and the following mirrors. The contact is
where that segment meets the mirror plane. This equals the chained image
construction but keeps each contact a single quotient, so bounds over a
parameter box do not compound.

**Corner elimination.** Corners with fixed XY have affine height equations.
Their coefficients are carried symbolically through the image weights and
eliminated by interval Gaussian elimination, so only roof and terrain
parameters are searched; corner heights are then recovered together. Heights
are solved relative to the source height so that large elevations do not
cancel. Uncertain pivots keep the original corner equations for exclusion and
never prove absence by themselves. Each elimination entry is the intersection
of its direct interval bound and its mean-value form e(m) + e'(X)(X − m) at the
box midpoint m. This is an exact reduction of the same system, not a
frozen-height approximation.

**Root search.** The remaining parameter domain is covered with
source-relative, outward-rounded interval automatic differentiation and the
Krawczyk test:

- A box is **excluded** by a sign test on a component, by disjoint contraction,
  or when it provably puts a contact on the wrong side of a mirror or outside
  a face (1e-7 m margin), checked before any division.
- Strict inclusion **verifies a unique root**.
- Point Newton steps only propose roots or neighbouring edges; a failed
  proposal never proves absence.
- Roots on subdivision boundaries are isolated in verified neighbourhoods and
  their complement is covered.
- Singular or uncovered regions, or an exhausted budget, leave the search
  **incomplete** (see below).

**Necessary constraints.** Before the image quotients are used, simpler
conditions shrink or exclude boxes. A vertical-section anchor's neighbours,
unfolded into its section, must be collinear with it in plan. For a lateral
anchor P, each mirror end A or B of its section is a line–plane intersection
kept as a polynomial with its signed-distance denominator dA or dB (1 at the
source or receiver); the lateral residual multiplied by dA³·dB² is then a
polynomial in the edge parameters, with the same zeros at any real route.
These constraints can exclude or narrow boxes but never certify a root; they
are needed because nested image quotients can be unbounded where no reflection
exists.

**Edges lying in a section.** A straight roof edge lying wholly in a lateral
plane (within 1e-7 m) has its endpoints as the route vertices, so its sliding
support is replaced by its endpoint corners. Likewise, a support edge whose
endpoints both lie within 1e-7 m of a vertical section's plane makes the
equation zero along the whole edge; such boxes are excluded and the support is
replaced by two supports pinned at the edge's endpoints. Pinned supports are
not unknowns, though their section equation must still hold. When both ends of
a vertical section are fixed on the edge's plan line, the edge goes directly to
its pinned endpoints. A vertical section using the same free edge twice is
replaced by the single merged support and by the pair pinned at both endpoints
(both orders).

**Topology discovery.** Which edges a route uses is discovered separately from
solving a fixed edge sequence. Initial section searches and vertical profiles
seed a queue; certified contacts recut the lateral chains and rebuild the
vertical profiles; reaching the end of an edge proposes the adjoining roof,
corner or terrain edges. Blocked provisional anchors serve only to discover
supports and carry no energy; a provisional mirror height outside a wall's
extent must not let that wall's own roof hide a controlling terrain ridge.
Each distinct edge sequence is solved once per initial section role and side;
geometric root covers are reused across octave bands (scaling a row by a
positive constant does not change its zeros), while band-dependent checks run
for every band.

**Accuracy.** Verified roots are refined until every 3D diffraction and
reflection contact is enclosed within **0.01 m**. This is an engineering
control on contact position, not an ISO tolerance or a bound on acoustic error.
It is checked after each contraction step; refinement stops after 260 steps
for contact width and 480 for edge bounds, and stalling first counts as
unresolved. The point estimate is refreshed inside the final enclosure, and
edge parameters are clamped before final validation. Edge predicates use their
own metre tolerance, including a narrow endpoint strip.

A solved route is accepted only if it passes the full checks: finite faces,
every-leg clearance, mirror side and specular direction (1e-6 as the cross
product of unit directions, plus 64 ulps of the largest coordinate per metre of
the shorter leg, so rounding in projected coordinates cannot reject the
micrometre-spaced contacts of a corner reflection), the lateral
wavelength, side, factor-eight and convexity rules, the controlling upper hull
of each vertical profile, and an unchanged support structure. Upper-hull
collinearity is tested as a middle point's excess height (1e-8 m), not a
cross-product bound that would scale with profile length; supports within
1e-9 m along a profile are one station at the highest of their heights, and a
terrain breakpoint at a footprint edge never replaces a higher wall or screen
top at the same station. For each initial section role and side, the shortest
fully valid route is kept.

### Limits and incomplete searches

The search allows 16 moving supports, 256 discovered edge sequences per
initial choice and 20,000 interval boxes per sequence. Reaching a limit, or
leaving any region unresolved, makes the search incomplete; work is never
silently dropped.

For a point-source family, an incomplete search is bounded: the family total is
capped at its unscreened reference (see *Total-contribution cap*), so routes the
search may have missed add at most the cap minus the capped energy of the
routes found. That bound is reported as omitted energy. If a receiver's summed
bound in any band exceeds the absolute tolerance plus the relative tolerance
times its computed and cut-off energy (the integration tolerances, defaults
1e-12 and 1e-3), the receiver carries a bounded warning (engineering choice).
Integration samples of line and area sources are not complete families, so
their incomplete searches are failed pathways.

**Failed pathways.** A `calculation_failed`, `nonfinite_result` or
incomplete-search error in one source/receiver/family, including while
preparing the family's image reference during the beam search, excludes that
family's energy; the receiver is marked incomplete with an unbounded warning
and the calculation continues. An extended-source boundary allowance above the
source–receiver tolerance is a bounded warning and keeps its trial energy. All
other errors stop the calculation: invalid input, unsupported features,
cancellation, source-level search failures and result-size limits.

Topology discovery is checked by supplementary cases; exhaustive coverage of
edge sequences and discovery of the globally shortest route are not proven.

## Ground attenuation

### Ground reference

Each source position, receiver and family has one **ground context**, built on
its reference path with screening ignored: the source–receiver line for direct
propagation, or the specular image path through the family's faces. The path is
projected onto XY and sampled in travel order (a region crossed twice counts
twice). Ground regions do not restart at a reflection.

A_gr and D_omega from this context are shared by all of the family's
alternatives (vertical, lateral and mixed) and by its unscreened cap reference.
Lateral detours and diffraction-shifted contacts do not change the ground
context, although ground attenuation still applies to lateral contributions.
This follows ISO/TR 17534-3 5.7 and the barrier-absent ground basis of
ISO 9613-2:2024 7.4.1.

### General method

`general` uses ISO 9613-2 7.3.1, Table 3 and equations (10)–(13), including the
2024 geometry correction. Source, middle and receiver regions follow the whole
horizontal reference trace and use length-weighted ground factors from the
ground regions crossed. D_omega is zero. The standard intends this method for
approximately planar ground (including constant slopes); the engine also
evaluates it on irregular terrain, without flattening the terrain, as an
unverified engineering extension.

### Mean-path-height method

`mean_path_height` uses the simplified method of 7.3.2, for sound other than a
pure tone over porous or mostly porous ground, assessed as an A-weighted level.
The same correction applies in every octave band, and band results are marked
as a broadband approximation.

The mean height h_m extends the 2024 Figure 5 construction over the reference
path's straight legs (engine interpretation for bent routes):

```text
F_i  = integral along leg i's horizontal trace of (reference-path elevation − terrain elevation) dx
d_g_i = 3D distance between the terrain points below leg i's endpoints
h_m  = sum_i(F_i) / sum_i(d_g_i)
```

For a straight path this is F/d_g. The areas F_i are exact trapezoids between
terrain breakpoints, path and terrain being interpolated separately. Added
terrain samples and collinear vertices do not add legs to the denominator;
legs with no horizontal span add no area; a zero total denominator is an
unsupported degenerate geometry. The reference path is a calculation reference,
not a physical route, so it may dip below terrain: those signed differences
are kept, not clipped.

With lengths in metres and F in square metres:

```text
A_gr    = max(0, 4.8 − (2 * h_m / d) * (17 + 300 / d))
K_geo   = (d_p² + (h_s − h_r)²) / (d_p² + (h_s + h_r)²)
D_omega = 10 * log10(1 + K_geo)
```

d is the full reference or image length, d_p its horizontal trace length, and
h_s, h_r the physical source and receiver heights above terrain. D_omega
(equation 15) is kept separate from source directivity and included once.
Screening coupling uses A_gr itself, not A_gr − D_omega. The general-method
correction and ground images are not added. G does not enter the formula,
though the method's applicability conditions still apply.

## Route level

For each source position, receiver, family and band, each valid route's level
is:

```text
A_bar   = max(0, V − max(A_gr, 0)) + H
L_route = L_W + D_c + D_omega − A_div − A_atm − A_gr − A_bar − A_refl − A_misc − C_met
E_route = 10^(L_route / 10)
```

V is the route's capped vertical diffraction loss (for a mixed route, the sum
of its capped vertical section losses; for an all-vertical reflected route, the
single unfolded loss), and H the sum of its lateral section losses. Positive
ground attenuation is subtracted from V once; H is unchanged, and a negative
A_gr keeps its reinforcement. Spreading and source power never restart at a
reflection or diffraction point.

| Term | Definition |
| --- | --- |
| L_W | Total point-source power, or unit power for an extended-source sample (cell power and quadrature weights are applied afterwards). |
| D_c | Source directivity in the actual launch direction, once. |
| D_omega | Mean-path-height ground gain, once; zero for the general method. With D_c it forms the ISO directivity correction. |
| A_div | 20 log10(d / 1 m) + 11, with d the full 3D reference or image distance. |
| A_atm | ISO 9613-1 absorption coefficient at the exact band centre frequency (2024 7.2) times the same distance. |
| A_refl | Sum of −10 log10(1 − absorption[b]) over the route's reflections. |
| A_gr | The selected ground method on the family's ground context. |
| A_bar | Screening as above. |
| A_misc | Foliage, site or housing attenuation where applicable, without double-counting explicit objects. |
| C_met | One correction per physical source, receiver and family (below). |

Each term keeps its own distance: diffraction uses the screened geometry and
its excess over the reference, ground the horizontal reference trace, and C_met
its fixed source-centre or image distance. A diffraction detour does not
lengthen every term. Near-source reflections are not counted both as explicit
paths and in source directivity.

### Total-contribution cap

For one source position, receiver, family and band:

```text
E_screened = sum of the energies of the distinct valid routes
E_accepted = min(E_screened, E_unscreened_reference)
```

The unscreened reference keeps source power, weather, ground, materials,
reflection contacts, face and size eligibility and other attenuations, but
has no screening loss, no obstruction tests and no host-face launch mask (an
inward reference must not erase a real route that bends round a building
corner). It is a ceiling, never added energy. A reference proven ineligible has
zero energy and so suppresses the family's band; numerical failure is never
replaced by a zero ceiling.

The cap is applied before cutoffs and totals; for extended sources, to each
point transfer before quadrature weighting. Different families are not capped
against each other. The cap extends the nonnegative-screening principle of
ISO 9613-2 7.4.4 (engine interpretation).

### Long-term meteorological correction

Each physical source gets one set of descriptors, fixed before any
subdivision:

- **Centre.** A point's acoustic position; the area- or length-weighted
  centroid of a uniform area or line. For zones of different power, the centre
  is weighted by A-weighted power: P_A_zone = sum_b(W_zone[b] * 10^(A_weighting[b]/10)),
  centre = sum(P_A_zone * centre_zone) / sum(P_A_zone). Silent zones have no
  weight; a silent source needs no correction.
- **Height.** The actual height above ground when constant, otherwise a given
  finite, nonnegative predominant height, separate from the centre's elevation.

d_p is the horizontal distance from the centre to the receiver, or for
vertical-wall reflections from the centre's image through the family's planes.
h_s and h_r are the physical heights.

```text
C_met = 0                                   if downwind or d_p <= 10(h_s + h_r)
C_met = C_0 * (1 − 10(h_s + h_r) / d_p)      otherwise
```

C_met applies once to every sample, route and cap reference of the family and
never again to totals; all bands share it, and detours, subdivision,
visibility and pruning do not change it. Centroid weighting and the reflected
distance are engine interpretations. Long-term calculations on irregular
terrain use the same descriptors and the receiver height above the actual
terrain (unverified extension). Long-term propagation via nonvertical
reflections is not supported.

## Line and area sources

Families are enumerated over each source domain using image transforms and
conservative domain windows; overlapping windows of the same family are
united, never duplicated or discarded. Each quadrature sample resolves its own
contacts and complete point transfer.

For each cell, receiver, family and active band:

1. Prove the whole cell impossible where possible; otherwise keep it. Required
   acoustic support is validated before any omission shortcut.
2. Split the cell at relevant transitions (source, aperture, screening, terrain,
   ground, foliage, directivity), preserving actual measure and zone power.
3. Require representative propagation across the cell and the extent/distance
   criterion: for areas a factor of at most 0.5, and any explicit maximum
   extent. A reflected image distance alone does not prove uniform conditions.
4. Compare coarse and refined positive, power-conserving quadratures per band.
   Accept the refined estimate when it meets the energy tolerance; otherwise
   subdivide deterministically.
5. Where geometry stays uncertain, refine further. Bands are accepted
   independently, so a jump in some bands (such as reflector eligibility) does
   not keep refining the others.
6. When the depth, floating-point subdivision or live-cell limit is reached,
   each held cell keeps its trial energy and its upper bound U. The family's
   unresolved allowance (accepted-cell discrepancy plus U) must fit within the
   tolerance of the complete source–receiver energy (all families, including
   cutoff omissions); otherwise the calculation fails.

Cells are classified (engine interpretation) as:

- **Impossible**: no contact can exist. A contact lies between source and
  receiver image only if they are strictly on opposite sides of the face plane;
  signed distance is affine, so the cell's vertices decide this. Contacts that
  all fall outside one edge of a convex face also prove absence.
- **Boundary**: the cell straddles a transition plane, its contacts cross a face
  edge, or the equation (26) size test is not provably constant for a band
  (bounded with the same effective size as point routes, `min(a cos, h cos)`).
- **Obstructed**: no transition is proven inside the cell, but obstacles,
  foliage or terrain along its reference paths keep the screening topology
  from being proven constant. These cells are accepted by the coarse/fine test
  and the extent/distance criterion, as ISO 9613-2 clause 4 allows for
  representing source parts by points, and are reported separately
  (`obstructed_topology_cells`, `obstructed_topology_energy`).
- **Smooth**: none of the above.

The upper bound U of a cell includes the material factor, maximum directivity,
a −6 dB net ground allowance, and divergence and absorption at the cell's
minimum distance to the receiver image; it is scaled up where child measures
exceed the parent's by rounding. At the live budget of 50,000 cells, cells
with the largest share of bound to tolerance are split first and the rest are
held. A split that would leave no representable area on one side keeps the
sliver whole.

Agreement at centroids, vertices or quadrature nodes cannot prove that a
narrow audible region is absent, and coarse/fine agreement is a refinement
test, not a certified error bound. Accepted cells accumulate per source,
receiver, family and band across all zones; once a family's bands are
complete, its cutoff is applied and its energy committed once. Subdivision
replaces a parent's work rather than adding to it. Cutoffs apply to complete
physical sources and families, never to cells or samples.

## Pruning and omission accounting

Pruning is a speed control, not part of ISO 9613-2. It skips work on paths
whose energy provably cannot matter, and never discards a contribution already
calculated. Without a valid bound, the path is evaluated. A numerical failure
or missed sample is never treated as zero.

### Pruning strategies

`pruning_strategy` names the weighted level to protect: `"dba"` (A-weighting)
or `"dbc"` (C-weighting). The calculation then runs one task per source and
reflection order: order 0 evaluates direct and screened paths without pruning,
and order k only families with exactly k reflections. Tasks start in order-major
sequence as workers become free; no order waits for the previous one. Once
every source's order-0 task has finished, each later task takes, when it
starts, the cutoff:

```text
L_q      = lowest weighted level, over receivers with energy, of all contributions completed so far
margin   = 10 log10(number of original sources) + c,   c = 15 dB (dBA) or 20 dB (dBC)
cutoff_b = L_q − margin − W_b          (W_b: A- or C-weighting of band b)
```

A band contribution is skipped when its upper bound is below `cutoff_b`, so its
weighted level is at least the margin below the quietest receiver so far.
Because calculated energy is kept, L_q only rises, and tasks started earlier
use lower, more cautious cutoffs. A task that starts before every direct path
is complete, or while no receiver has energy, runs unpruned. L_q may include
higher-order energy from other sources. Because cutoffs depend on which tasks
have finished, pruned results can differ slightly between runs; unpruned
results do not.

The margin is empirical, not a guarantee: many skipped contributions can add
up, and their number grows with the number of sources, hence the
10 log10(N) term. dBC needs the larger constant because its total is set by the
loud low bands. Individual octave bands, especially quiet ones, can lose more
than the weighted total.

`minimum_contribution_db` applies one fixed absolute cutoff in every band
instead.

### Where bounds are tested

Each test uses the band cutoff in force; a family without a valid bound below
the cutoff is calculated in full.

- **Reflection prefixes.** Before a beam continues through a reflector, each
  active band is bounded for every continuation at every receiver: source
  power, maximum directivity, a +6 dB net ground allowance, the reflectors'
  material losses, and divergence and absorption over a lower bound on route
  length (the distance from the previous image domain to the reflector plus the
  distance from the reflector to the nearest receiver). A band also becomes
  inactive when the face cannot meet the equation (26) size test for any
  continuation, its effective size being at most the face's horizontal width
  and vertical extent. Screening, meteorology and miscellaneous losses are
  taken as zero. Child beams inherit the active bands; a beam with none stops.
- **Point families with only vertical reflectors**: per band, the unscreened
  family cap times source power, before routes are built. Families with a
  nonvertical reflector are pruned only at the prefix test.
- **Line and area families**: per band, the sum of the cell bounds U over every
  zone cell, before partitioning or quadrature.

For a cell, U = W_cell * T_upper, where T_upper bounds the combined transfer of
all eligible alternatives throughout the cell; distance is bounded over the
whole image domain, and a lower bound of zero stays zero. Unknown net ground
attenuation A_gr − D_omega is replaced by a single −6 dB allowance (D_omega is
not added again): conservative for general ground under its nonnegative-height
assumptions, and for the simplified method where A_gr >= 0 and
D_omega <= 10 log10(2). Bounds cover both the integral and the permitted
quadratures.

Bands pruned at a receiver connection report their bound as omitted energy;
beams pruned at a prefix never reach a receiver and report nothing.

## Execution

Sources, receiver blocks, reflection orders and families are independent
parallel work; each source–receiver pair is calculated within one work item.
Energies are summed with exact, correctly rounded sums, so results do not
depend on order, partitioning or worker count (except with pruning, above).
Lists such as warnings follow a canonical order: reflection order, then source,
then family.

Scene preparation, source geometry and reflection prefixes are shared and
immutable. Each beam carries its own copy of the active-band mask; silent,
fully absorbed or omitted bands are removed and never restored downstream, and
a candidate stops only when no band remains. Adaptive decisions and ties are
deterministic. Cancellation is checked throughout; there is no
operation-count ceiling, and failed, cancelled or incomplete work never
exposes a completed result. Progress counts one point for preparation and
1, 2 or 3 points per original point, line or area source (API.md).

## Results

```text
L_receiver[b] = 10 log10(sum over sources of E_source[b])
L_source[b]   = 10 log10(E_source[b])
L_A           = 10 log10(sum_b(E_receiver[b] * 10^(A_weighting[b]/10)))
```

Levels are dB re 20 µPa; L_A is in dBA, with A-weighting applied once to the
total. Zero energy has no finite level and is reported as such, never as 0 dB;
proven silence is distinguished from cutoff-limited output. A band cutoff does
not imply a floor on the dBA level.

Results include the method editions and profile, resolved settings, receiver
and per-source levels, grid identities, integration diagnostics and omission
coverage. Omitted energy O gives the interval [E, E + O]: for E > 0 the upper
level increase is 10 log10(1 + O/E); for E = 0 and O > 0 an absolute upper
level is given. Heuristic cuts, the reflection-order limit, integration
uncertainty and unsupported effects are reported separately. The interval
covers only the omissions within the calculated path set; it is not a bound on
the physical prediction.

## Engine interpretations and exclusions

Where ISO 9613-2:2024 does not prescribe a construction, the engine uses these
interpretations:

- Building-surface sources: supplied emission data apply unchanged, with no
  3 dB solid-angle correction or host reflection, and the outward launch test.
- Reflected screening: all-vertical routes per the DAGA proposal; mixed routes
  as the sum of section losses.
- Mean path height on bent routes: the leg-wise extension of Figure 5.
- The total-contribution cap on the unscreened reference.
- Long-term meteorology: weighted source centres and image distances.
- Extended-source cell classification and bounds.
- The general ground method and long-term correction evaluated on irregular
  terrain.

Excluded from scope:

- The special long-term industrial-screening treatment of ISO 9613-2:2024
  7.4.1, Note 1.
- Screened propagation via nonvertical reflectors, and long-term propagation via
  nonvertical reflections (reported as unsupported).

[mixedscreening]: https://pub.dega-akustik.de/DAGA_2026/files/upload/paper/659.pdf
