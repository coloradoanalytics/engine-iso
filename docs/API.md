# ISO calculation engine API

Contract revision **1.1 (shared implementation target)**, 2026-10-08. This is
the request and result contract of the ISO calculation for the QGIS frontend
and the maintained Rust engine, engine-iso. [ALGORITHM.md](ALGORITHM.md) defines the calculation; this
document defines the data needed to request it and interpret its results.

**Transport (2026-10-16).** engine-iso has no network interface. Clients reach
it through [acalc-engine](../../acalc-engine/README.md), which serves the
HTTP routes, keys, job states, events, results, artifacts, retention and
status codes the same for every engine:
[acalc-engine docs/API.md](../../acalc-engine/docs/API.md). The routes and
behaviour of contract 1.1 are unchanged for clients; acalc-engine passes request
documents to engine-iso unchanged
([engine protocol](../../acalc-engine/docs/ENGINE_PROTOCOL.md)). Transport
sections below are kept short and defer to acalc-engine.
The target is **ISO 9613-2:2024** with **ISO 9613-1:1993** atmospheric absorption.
Edition applicability, independent evidence and conformity claims are kept
by the maintainers' quality record, not by this contract.

Revision 1.1 adds optional, capability-gated inputs to `/v1`. Existing requests,
defaults, coordinate arrays and null meanings retain their meaning. New fields
must be advertised before a client sends them; an older decoder may reject them.
A change to an existing field's meaning requires a new route version.

The schemas cover the full algorithm's input needs, including preparation that
cannot be performed correctly by sending only acoustic XYZ. They are **not a
release or verification claim for the Rust service or the current QGIS adapter**.
A present unsupported effect is an error. Parser acceptance, method identity
and acoustic conformity are separate claims.

Go implementation details and comparisons below are historical evidence for the
retired Go engines. Rust live capabilities and its evidence
records define current implementation scope; historical Go coverage is not Rust coverage.

## Address and conventions

Default base URL (acalc-engine): `http://127.0.0.1:8343`. Routes below are
relative to it. `/v1/capabilities` and `/v1/calculations` reach the default
engine, `iso`; `/v1/engines/iso/...` names it explicitly.

- UTF-8 JSON; requests containing JSON use `Content-Type: application/json`.
- JSON names use `snake_case`. Objects are unordered; arrays retain order.
- All numbers must be finite. JSON `NaN`, infinities, numeric strings and
  booleans in numeric fields are invalid. XYZ coordinates require exactly three
  numbers; explicitly labelled XY coordinates require two. Spectra require
  exactly eight entries.
- Reject unknown request fields, invalid types, unsupported effects and
  incompatible settings. Never silently discard an acoustic input.
- Omission selects a documented default. `null` is accepted only where specified
  below; it does not generally mean omission. Empty required arrays are invalid.
- IDs are nonempty opaque strings. Source IDs are unique among physical sources,
  zone IDs within their source, obstacle IDs across obstacle collections, and
  receiver IDs among receivers. Clients must retain IDs rather than infer
  identities from a completion order. Calculation IDs are allocated by the
  service and must not be interpreted by clients.
- Timestamps are RFC 3339 UTC strings. No client/server clock synchronization
  is needed to calculate progress.
- Responses may gain additional fields within version 1. Clients ignore unknown
  response fields. Breaking changes require a new route version. An engine
  accepting additional input fields must advertise that capability first.
- Authentication (API keys), browser-origin rejection and listener addresses
  are acalc-engine's; see its API.md.

## Endpoints

acalc-engine's API.md lists the routes: capabilities, submission, status,
events, results, artifacts and cancellation, plus `/healthz` and API keys.
A submission is not an idempotent operation: retrying POST creates another job.
Follow the returned links; do not assume a calculation ID format or construct
filesystem paths. Malformed or invalid submissions do not create jobs: engine-iso
validates each request before acalc-engine creates the job. A valid submission
can still fail during geometric evaluation or integration; that failure is
reported in its terminal snapshot.

## Capabilities and edition identity

acalc-engine's `GET /healthz` reports the default engine's `method` and
`atmosphere` (`"ISO 9613-2:2024"`, `"ISO 9613-1:1993"`); `engine-iso --describe`
prints the engine's own identity, including its `build_revision`.

The capabilities response contains:

| Field | Type / meaning |
| --- | --- |
| `api_version` | String; major version `1`. Revision `"1.1"` identifies this expanded contract, not support for every feature. |
| `algorithm_revision` | Immutable revision/hash of the implemented algorithm, distinct from implementation version and ISO edition; required for extended profiles. |
| `features` | Supported feature identifiers below. Absence does not imply support for new fields. |
| `profiles` | Supported combinations: each has `id`, `features`, `ground_methods`, `meteorological_modes`, `restrictions` and `verification_status`. |
| `component_profiles` | Extended array of `{id, revision, category, input_schema, restrictions, verification_status}` for terrain construction, attachment placement, directivity conversion or miscellaneous attenuation. `input_schema` is a self-contained JSON Schema for that component; revision is immutable. No remote schema fetch is necessary. |
| `ground_methods` | Supported subset of `"general"`, `"mean_path_height"`; legacy omission means `general` only. |
| `implementation` | Object with `name` and `version` strings. |
| `method` | `"ISO 9613-2:2024"`. |
| `atmospheric_absorption` | `"ISO 9613-1:1993"`. |
| `quality_guidance` | Array of reviewed QA documents/edition qualifications. |
| `verified_conformity` | Boolean; `false` in the initial Go implementation. |
| `frequencies_hz` | `[63,125,250,500,1000,2000,4000,8000]`. |
| `source_kinds` | Supported kinds; `iso_go` supports `["point","line","area"]`. Combinations remain subject to profile restrictions. |
| `receiver_kinds` | Initially `["point","area"]`; native area grids expand during preparation. |
| `coordinates`, `ground` | Descriptions of supported coordinate and ground models. |
| `max_reflect_order` | Largest accepted reflection order; `iso_go` 0.4.0 supports `5`. |
| `pruning_strategies` | Optional. Weighted levels accepted by `settings.pruning_strategy`; Rust lists `["dba","dbc"]` and the feature `pruning_strategy`. |
| `progress_task_points` | `{"preparation":1,"point_source":1,"line_source":2,"area_source":3}`. |
| `limitations` | Array of scope and verification qualifications. |
| `profile_limits` | Object advertising numerical/resource limits; see the settings section. |
| `transport` | Added by acalc-engine: `max_request_bytes`, `max_retained_jobs`, `event_interval_ms`, `job_persistence`, `event_replay` and more (its API.md). engine-iso's own document (`engine-iso --capabilities`) has none. |

Capability descriptions must identify restrictions, including supported
combinations. Accepting a kind named `area` does not imply support for reflected
or screened area integration. Implementations must not reuse 1996 reference
results as 2024 validation without an applicability review.

The shared target uses the algorithm's reference-ground context, manual ground
method selection, physical-source meteorological descriptors and contribution
cap. Live capabilities describe which combinations a backend implements. Do
not infer support from its language, another implementation, this schema or
work-in-progress source structures.

### Extension discovery and compatibility

| Feature identifier | Fields / behavior requiring it |
| --- | --- |
| `line_sources`, `emission_zones` | Line geometry, per-metre power and original-source zones. |
| `terrain_samples` | Ground samples/breaklines, engine-built terrain and terrain-relative points. |
| `terrain_inference` | Named inference targets, `ground_ref`, indexed `geometry`, staged surface construction. |
| `mixed_ground` | Material regions and an explicit overlap/boundary policy. |
| `building_solids`, `building_roof_mesh` | Solid buildings; separately, explicit nonplanar roof triangulation. |
| `building_attached_sources` | Host/face identity, building-local placement and emission rules. |
| `polyline_barriers`, `reflecting_barriers` | Bent/sloping screens; separately, reflection on the same wall geometry. |
| `planar_reflectors` | General finite planar surfaces beyond vertical rectangles. |
| `directivity_tables` | Complete octave correction grids, interpolation and orientation. |
| `directivity_orientation_matrix` | Direct transmission of the model's proper local-to-world rotation. |
| `roof_reflections`, `nonvertical_reflections`, `nonvertical_reflected_long_term` | Separately qualified face/reflection and meteorological combinations. |
| `foliage_simple`, `industrial_regions`, `housing_regions` | Corresponding `misc_regions` schemas and named attenuation profiles. |
| `mean_path_height` | Manual ground-method choice and applicability declarations. |
| `extended_long_term`, `reflected_long_term`, `irregular_terrain_long_term` | Corresponding meteorological combinations and source descriptors. |
| `reflected_screening`, `screened_extended_sources`, `reflected_extended_sources` | Combined propagation, not just acceptance of the separate object kinds. |
| `prepared_geometry`, `omission_bounds`, `path_diagnostics` | Resolved geometry, certified omission coverage or optional detailed route ledgers. |
| `integration_reports` | `output.integration_reports`: per-zone extended-source integration reports on request; a summary otherwise. |

`profiles[].restrictions` is an array of scope qualifications;
`verification_status` is `unverified`, `partially_verified`, or
`verified_within_declared_scope`, with evidence in `quality_guidance`.
A feature must name an executable combination; it cannot imply every ground,
source, obstacle, meteorology and attachment combination. Reflection order zero
still needs screening/contact support.

Extended requests may include `profile_id` to select an advertised combination;
omission selects the documented backend default. Both backends must use the
same algorithm revision and profile semantics to be compared as implementations
of the same model. `profile_limits` must state any limits on sources, zones,
vertices, triangles, terrain samples/inference points/breaklines, solids/faces,
regions, grids and families. Limits fail work; they never authorize
coarsening a grid or dropping an effect.

## QGIS GeoPackage boundary

The retired Go subset's precise numerical choices were recorded with that
engine. Its advertised profile
restricts the broader optional forms in this shared contract.

**The frontend must submit the equivalent normalized model through this JSON
API.** Version 1 does not accept a GeoPackage upload, database connection, file
path or raw GeoPackage geometry blob. The engine must not infer that a path is
accessible on its own machine. The plugin remains the GeoPackage reader and
result writer; language implementations share this normalized contract.

The current model contract is [GEOPACKAGE.md](../../acalc-qgis/docs/GEOPACKAGE.md), schema 11
as defined in `acalc/schema.py`. Validate the stored schema version before
adapting it. An adapter must account for every calculation-relevant selected
object and setting. If the API cannot represent an effect, stop with an
object/setting-specific incompatibility. A smaller supported scene is not an
equivalent conversion. Do not flatten terrain, strip buildings or turn a line
source into unrelated points to get a successful submission.

QGIS submits explicit ground samples from all selected object types that carry
valid ground Z, including buildings, receivers, terrain points and terrain
lines. The engine constructs the triangulated surface. Acoustic XYZ includes
height and must not be used as a ground sample. Unresolved `infer_z` placeholders
are excluded. Receiver-area perimeters and generated points do not seed terrain.
Raster-to-mesh construction is deferred. Use the staged ground-reference forms
below for unresolved geometry; do not pre-resolve it with a different terrain
model and relabel it as measured ground.

| GeoPackage data | Required normalized API treatment |
| --- | --- |
| `acalc_project` | Read schema version, project ID, CRS WKT, `horizontal_to_m`, vertical datum and revision information. Verify the projected metric coordinate interpretation. Preserve provenance in the frontend run record. |
| `acalc_variants.definition_json.layer_ids` | Resolve exactly the selected variant's layers through `acalc_layers.layer_id`, `table_name`, `feature_type`, with `archived = 0`. Read those actual feature tables, usually named `model_<UUID hex>`. Layer visibility, current selection, QGIS FIDs and legacy `enabled` fields do not filter the calculation. |
| Feature identities | Namespace each API ID by project/layer/object as necessary to preserve unique identity. Save the reversible mapping; don't use a changing row index as an object ID. |
| Point/line/area `geom` coordinates | GeoPackage X/Y are project units; multiply by the verified horizontal-to-metre factor. Z is ground elevation in metres and M is acoustic height in metres. After ground resolution, API absolute acoustic Z is `ground_Z + M`. Do not treat M as another XY unit or Z as already including height. |
| `infer_z`, ground modes, terrain inputs | Send explicit ground XYZ/breaklines plus named `terrain.inference_points` for original inferred vertices. Use `ground_ref` and indexed `geometry` to preserve staged inference for sources, receivers, buildings and walls. `position_xy` + `height_m` is for final-terrain sampling that does not add a terrain vertex. |
| `point_source` | Join `spectrum_id`, normalize its spectrum, emit `kind: point` and an absolute or ground-referenced location. |
| `area_source` | Preserve original ID and actual 3D emitting domain with an explicit nonoverlapping triangle mesh. Respect the established surface definition; reject ambiguous nonplanar polygon triangulation. Map catalog power basis as below. |
| `line_source` | Send `kind: line`, acoustic `vertices`, original ID and `total_power` or `power_per_m` spectrum. Two task points belong to the physical line, independent of segmentation. |
| `point_receiver` | Submit absolute or ground-referenced location and stable ID. |
| `area_receiver` | Submit native `area_receivers` or equivalent pre-expanded receivers. Native grids sample prepared terrain and automatically exclude included building footprints. Ignore legacy perimeter Z/M and ground-mode fields. |
| `acalc_spectra.spectrum_json` | Require resolved forward `values`, sound-power quantity, unweighted octave bands, and correct units. Do not use adjustable `initial_values`. Under model spectrum version 2, numeric zero is silence and becomes API null; model null is unknown and is an error, **not** API silence. Preserve finite nonzero values, including negatives. |
| Catalog `power_basis` | `total` maps to `total_power`; `per_square_metre` to `power_per_m2`; `per_metre` to `power_per_m`. Normalize only once. |
| Material assignment | Resolve `absorption_spectrum_id` first, then object `reflection_coefficient`, then the applicable building/barrier default. For scalar energy reflection coefficient rho, send absorption `1-rho` in each band. Explicit rho=0 is fully absorbing, not missing. Unknown assigned material values are errors even at reflection order zero. |
| `barrier` | Preserve original base ground, per-vertex tops, screening/reflection role and material. A compatible rectangle can use `reflectors`; extended walls use the barrier schema below. Do not invent measured surface density or submit the same wall twice to obtain reflection. |
| `building`, roof, `height_mode`, attached `face_sources` | Submit the solid, ground references, roof rule/elevations/mesh and stable host/face IDs. Preserve absolute roof Z+M before inference. Use `attachment` for host-dependent geometry; never export facades without the solid or add a +3 dB host gain. |
| `ground_absorption`, `trees`, nonflat `terrain_point` / `terrain_line` | Ground polygons map to `ground.regions`; terrain vertices/breaklines map to `terrain`. Foliage remains subject to discovered support; do not discard selected trees. |
| Assigned `directivity_id` and orientation | Resolve executable catalog data into the source/zone correction table and orientation. Raw native-frequency/relative-power/draft catalogs require explicit conversion and normalization evidence; do not treat them as ready corrections or replace them with omnidirectional emission. |
| Measurements / elevation raster references | Schema-11 Measurements are observations, not forward acoustic/terrain objects; their stored Z does not become terrain merely by sharing a point geometry type. Raster references remain authoring provenance; sampled ground already saved on selected elevated objects contributes normally. An independently intended terrain observation must be represented explicitly as a terrain object. |

Settings require explicit mapping rather than passing the old settings object:

- Temperature, relative humidity, pressure and uniform ground factor map to
  the environment fields above. `meteorology` maps to `meteorological_mode`;
  `c0_db` maps to `c0` for long-term calculations. A saved dormant downwind C₀
  remains in the frontend model; send zero for an actual downwind run.
- `reflection_order` maps to `max_reflect_order` only within the discovered capability limit (0–5 in `iso_go` 0.4.0). The plugin's
  default is 3, while the API default is 0: submit the selected value explicitly.
- `reflection_search_distance_m`, `line_segment_max_m`, and
  `subdivision_tolerance_db` have no identical API meaning. In particular,
  the old subdivision dB tolerance is not `area_relative_tolerance`. Report an
  active incompatible setting or have the user explicitly select and save a
  new backend-specific setting. A provably inactive setting can remain solely
  in provenance, for example reflection search distance with no reflections.
- The plugin's `lateral_diffraction: fresnel` is not this ISO profile. Reject
  it rather than silently using the ISO setting.
- The plugin offers pruning only as a strategy (`pruning_strategy` dBA or dBC)
  and always sends `minimum_contribution_db: null`. It sends `pruning_strategy`
  only when pruning is on, and refuses an API whose capabilities do not list the
  chosen strategy in `pruning_strategies`. Save all newly chosen numerical settings
  with the backend identity; don't silently replace a saved calculation setup.

Persist the exact submitted snapshot, normalized settings, backend/version,
variant membership and the ID mapping with the completed run. Convert returned
XYZ back to project XY units and the GeoPackage ground/height representation
using the saved mapping. Only completed results may replace the variant's
previous results. Failures/cancellation leave earlier results intact; a valid
completed empty receiver grid intentionally replaces them with an empty set.
Changing the model while a job runs must not make its results appear current
for a different model revision.

## Calculation request

Every POST body is one JSON object. `sources` is a required nonempty array.
At least one explicit `receivers` entry or `area_receivers` definition is required.
Either receiver array may be omitted; an area-only request is valid, including
a defined area whose exclusions leave an empty grid. `atmosphere`, `ground` and `settings` may be omitted; if supplied,
they must be objects. `reflectors`, `barriers`, `buildings` and `misc_regions`
default to empty arrays. New extension objects/fields require declared support.
Unspecified effects have zero contribution only when they are absent from the
scene; unsupported objects cannot be discarded by the client or service.

Additional top-level fields in revision 1.1 are `profile_id`, `terrain`,
`applicability`, and `output`, as defined below. Explicit receivers are objects
with required `id` and exactly one point-location form. They have no spectrum
or source `kind`. Every source, zone, material, ground reference, host and
method/profile reference must resolve inside the submitted snapshot or the
advertised immutable profile set. The server cannot fetch a client's catalog
or GeoPackage to complete a missing field.

A complete, minimal runnable example:

```json
{
  "sources": [
    {
      "id": "fan-1",
      "kind": "point",
      "position": [0, 0, 1.5],
      "spectrum": [90, 90, 90, 90, 90, 90, 90, 90]
    }
  ],
  "receivers": [
    {"id": "receiver-1", "position": [100, 0, 1.5]}
  ],
  "atmosphere": {
    "temperature_c": 10,
    "relative_humidity_percent": 70,
    "pressure_kpa": 101.325
  },
  "ground": {"factor": 0, "elevation_m": 0},
  "settings": {
    "max_reflect_order": 0,
    "minimum_contribution_db": null,
    "meteorological_mode": "downwind"
  }
}
```

### Coordinates, spectra and sources

Coordinates are `[x,y,z]` in local Cartesian metres with one common vertical
datum. Z is the absolute acoustic position, not height above ground. The client
resolves QGIS CRS transformations, unit conversion and Z/height conventions.
A point may instead use `position_xy: [x,y]` and nonnegative `height_m`;
the engine adds prepared terrain elevation. These two fields are supplied
together and exclude absolute `position`. A source or receiver below terrain,
coincident point source/receiver, or receiver touching an emitting mesh is invalid.

Spectra are unweighted octave sound-power levels in dB re 1 pW, ordered by the
eight nominal frequencies above. **Numeric `0` is a real 0 dB level.** A `null`
band means explicitly zero emitted energy. Eight nulls describe a silent source;
a whole-spectrum null is invalid. A missing spectrum is accepted only when
every emission zone supplies its own complete spectrum. The frontend must
translate any legacy QGIS zero-as-silence representation explicitly.

Each physical source has `id`, `kind` and a `spectrum`, except in explicit
zone-power mode below. Extended geometry may use indexed `geometry` when
terrain/attachment preparation is needed. Additional fields:

| Kind | Fields |
| --- | --- |
| `point` | One location form below, or `attachment`; optional `power_basis` must be `"total_power"`; no extended geometry or zones. |
| `area` | One of `triangles`, indexed `geometry`, `attachment`, or `zones`; `power_basis` is `"total_power"` or `"power_per_m2"`; no point position. |
| `line` | One of `vertices`, indexed `geometry`, or `zones`; `power_basis` is `"total_power"` or `"power_per_m"`; no point position or raw triangles. |

A triangle is an array of three 3D vertices. Mesh triangle interiors must not
overlap or intersect; shared edges are allowed. Positive actual 3D emitting area
is used, including sloped and vertical faces. The client supplies the surface
mesh; the engine must not invent triangulation across an ambiguous nonplanar
polygon. Uniform spectrum applies per emission zone; absent directivity is
omnidirectional.
Hole boundaries and disconnected patches are represented by their nonoverlapping
triangle domains; no triangles may span excluded areas.

For `total_power`, the spectrum is the total for the complete mesh and is divided
by actual area. For `power_per_m2`, the spectrum is dB re 1 pW per square metre
and is integrated over actual emitting area. Refinement must conserve power and
retain the original source ID. Triangle count never changes progress weight.

Example area source (insert into `sources`):

```json
{
  "id": "panel-1",
  "kind": "area",
  "power_basis": "total_power",
  "spectrum": [80, 80, 80, 80, 80, 80, 80, 80],
  "triangles": [
    [[0,0,2], [2,0,2], [2,2,2]],
    [[0,0,2], [2,2,2], [0,2,2]]
  ]
}
```

### Ground references and unresolved emitting geometry

These additive forms require `terrain_inference`. An ordinary XYZ coordinate
always remains three finite numbers; **null Z is not a ground-inference flag**.

A `GroundRef` is exactly one of:

```json
{"position": [25, 10, 12.5]}
```

```json
{"inference_id": "receiver-ground-17"}
```

The first is explicit ground XYZ and participates in the initial surface. The
second references `terrain.inference_points[].id`; it has XY and obtains ground
Z from the immutable initial surface. A reference is not an acoustic position.
Only originally selected objects carrying ground elevations may create these
terrain supports; quadrature points and building-attached sources cannot.

Point sources and explicit receivers accept exactly one location form:

| Form | Fields and meaning |
| --- | --- |
| Absolute | `position: [x,y,acoustic_z]`. Supply any original explicit ground separately in `terrain.samples`; acoustic Z never seeds terrain. |
| Final-terrain relative | `position_xy: [x,y]` and required nonnegative `height_m`. Sample the final surface; this form creates no terrain support. |
| Original ground vertex | `ground_ref: GroundRef` and exactly one of nonnegative `height_m` or finite absolute `elevation_m`. Resolve the referenced original ground before obtaining acoustic Z. |

`position`, `position_xy` and `ground_ref` are mutually exclusive. `elevation_m`
is an absolute acoustic elevation, including negative elevations in the common
datum; it is not an extra height. No form changes terrain from acoustic Z.
Reject physical locations below terrain or unsupported contacts. Results always
return resolved absolute `position`, plus `ground_elevation_m` and `height_m`
for extended profiles, allowing the frontend to reconstruct its Z/M convention.

Line/area sources or zones may use `geometry` instead of raw `vertices` or
`triangles`. `geometry.vertices` is a nonempty array of the location objects
above. For a line it is the ordered polyline. For an area,
`geometry.triangles` contains triples of distinct zero-based vertex indices,
covering exactly its emitting domain. Indices are topology within this immutable
snapshot, not replacement source/object IDs. Mesh interiors must not overlap;
unused vertices and degenerate triangles are invalid. A source cannot mix parent
geometry with zone geometry, or raw geometry with indexed geometry.

The client supplies the intended triangle connectivity for nonplanar unattached
areas, including when inference changes elevations. The engine resolves vertex
heights and validates that connectivity; it cannot choose a different diagonal
or flatten the source. Planar polygon authoring may be triangulated before
submission if the actual domain, holes and attachment identities are preserved.
No interpolation or subdivision vertex becomes an explicit terrain observation.

For example, this line retains its physical ID while both endpoint grounds are
resolved by the service:

```json
{
  "id": "pipe-1",
  "kind": "line",
  "power_basis": "power_per_m",
  "spectrum": [70,70,70,70,70,70,70,70],
  "geometry": {
    "vertices": [
      {"ground_ref": {"inference_id": "pipe-ground-a"}, "height_m": 2},
      {"ground_ref": {"inference_id": "pipe-ground-b"}, "height_m": 2}
    ]
  }
}
```

Both referenced targets must occur in `terrain.inference_points`. This is a
fragment: its containing request also needs
`terrain.construction_profile: "acalc_schema11"` and explicit initial ground
observations. It is not an independently verified acoustic case.

### Lines, emission zones and long-term descriptors

A line's `vertices` are an ordered acoustic XYZ polyline with at least two
vertices and positive segment lengths. `power_per_m` is power per actual metre,
including vertical distance. The physical source keeps one ID and two task
points. Lines currently share the `area_*` numerical refinement controls; these
control cells for both extended-source kinds, with positive quadrature weights.

An extended source can instead supply `zones`, each with a unique nonempty
`id` and the corresponding `vertices` or `triangles`. Do not supply geometry
both at the parent and in zones. Either all zones inherit the parent spectrum,
or every zone supplies its own `spectrum`; partly overridden spectra are
rejected. An inherited total is divided by actual zone measure. An inherited
density is integrated over each zone. Independently powered zones use their
own `power_basis`, falling back to the parent's when omitted. Areas must not
overlap emitting interiors across zones.

An omitted effective `power_basis` means `total_power`. In inherited-power mode,
zones omit `power_basis` as well as `spectrum`; the parent's basis applies to
the complete source. In explicit-zone-power mode, omit the parent `spectrum`;
it is not an additional emitter or a second total to redistribute. Extended
profiles reject competing parent/zone power declarations. Each zone must have
positive actual measure even if silent. Zone boundaries and subdivision do not
change the source count, cutoff scope or progress weight. A zone may also use
indexed `geometry`; attached zones are covered below.

`predominant_height_m` is an optional finite nonnegative physical-source
height. For `long_term`, it is required when height above terrain is not
constant, except that a wholly silent source requires no correction. Point
centres use their position; uniform extended sources use actual
measure centroids. Heterogeneous centres are weighted by linear A-weighted zone
emitted power. The centre and height remain fixed across integration and visibility
changes. Use actual constant height when it is constant; a supplied predominant
height must not override it. Never derive predominant height from terrain under
the source centroid. For vertical reflecting planes, mirror the centre for the
family's horizontal Cmet distance while retaining physical heights.
Nonvertical reflections and irregular-terrain long-term applicability need
separately advertised profiles; selecting `mean_path_height` does not enable
them. These are descriptors of the original source, computed before subdivision.

### Source directivity and host emission

A source or zone may supply `directivity` with:

| Field | Meaning |
| --- | --- |
| `azimuth_deg` | 1–720 strictly increasing local angles, starting at 0 and below 360; periodic seam. |
| `elevation_deg` | 2–361 strictly increasing angles with endpoints -90 and +90. |
| `correction_db` | Elevation rows, azimuth columns, exactly eight finite corrections per cell. No null bands; range -200 to +60 dB. Each pole has identical values at all azimuths. |
| `orientation_deg` | `[yaw,pitch,roll]`, default `[0,0,0]`, each within +/-360 degrees. Local-to-world rotation is Rz(yaw) Ry(pitch) Rx(roll). |
| `orientation_matrix` | Extended alternative to `orientation_deg`: three rows of a finite, orthonormal 3×3 matrix with determinant +1, mapping local column vectors to world coordinates. The two forms are mutually exclusive. |

Local +X is azimuth 0/elevation 0; +Y is azimuth 90 and +Z is elevation 90.
Bilinear interpolation in dB and periodic azimuth wrapping are API numerical
conventions. Do not renormalize the supplied data. A zone override replaces its
parent's table. Evaluate direction on the actual first nonzero route segment;
apply the correction once. The hypothetical cap reference uses its own launch
direction and retains directivity.

World +X/+Y/+Z means project grid east/north/up after the frontend's metric
coordinate conversion. Preserve the model's source-local +X front, +Y left,
+Z up orientation. A client converting a stored matrix to yaw/pitch/roll must
preserve that rotation, including at Euler singularities; it cannot substitute
the catalog's principal direction. A backend accepting matrices advertises
`directivity_orientation_matrix` and its numerical orthonormality tolerance.

This grid is a **resolved correction**, not a raw directivity catalog. Finite
0 dB means no correction; null means missing data and is invalid here. A
schema-11 catalog may have 1–64 native frequencies, sparse measured directions,
relative-power values, unknown cells or an unparsed CLF attachment. Those are
not this grid. Conversion must resolve angular interpolation, native-frequency
to octave conversion and the correction's reference/normalization against the
assigned sound-power spectrum. Do not equate `relative_power` with `correction`,
extrapolate unknown bands, or use the preview's nearest-direction lookup as an
unstated acoustic rule. ALGORITHM.md does not define that conversion; raw
catalogs remain unsupported until a separately versioned conversion is agreed.

Extended `directivity` may carry `provenance` with string fields `catalog_id`,
`catalog_revision`, `quantity` (must be `correction`), `reference`,
`conversion_profile`, and `conversion_revision`. For converted catalogs retain
all of these with the original native data in the frontend input record. This
metadata explains the supplied correction; it never triggers renormalization
or another conversion on the service.

`host_building_id` identifies a source's building support. Resolve its outward
normal from the solid. Inward physical launch directions have zero energy;
outward and tangential launches retain the supplied power/directivity. Do not
add an automatic +3 dB correction, an offset, a taper or a source-coincident
host reflection. Planar attached areas must remain on a single face plane.
Untagged contacts need one unambiguous supporting face; coplanar seams are equivalent.

### Area receivers

`area_receivers` is an optional array. Each definition has:

| Field | Meaning |
| --- | --- |
| `id` | Required unique nonempty area ID; preserved as the parent link in results. |
| `boundary` | Required simple polygon as at least three `[x,y]` metre vertices. Either winding and concavity are supported. An optional repeated closing vertex is normalized. |
| `height_m` | Shared nonnegative height above ground, default 1.5 m. Perimeter Z/M are not accepted. |
| `spacing_m` | Positive metric interval on both axes, default 10 m. |
| `exclude_polygons` | Optional array of simple XY polygons whose interiors and boundaries remove candidate receivers. Default empty. These are sampling masks, not acoustic obstacle definitions. |

Example:

```json
{
  "id": "yard-grid",
  "boundary": [[100,0], [120,0], [120,20], [100,20]],
  "height_m": 1.5,
  "spacing_m": 10,
  "exclude_polygons": []
}
```

The axis-aligned grid starts at the boundary's minimum X and Y. Candidate
column/row `(c,r)` is `(min_x + c*spacing_m, min_y + r*spacing_m)`. Include
polygon edges using 1e-8 m distance tolerance; remove points outside the boundary
or inside/on an exclusion polygon. Include at most one upper-axis endpoint
within 1e-8 m beyond the bounding maximum to tolerate decimal-spacing rounding;
preserve the computed coordinate rather than snapping it. Reject spacing that
cannot advance coordinates at float64 precision. Iterate row first, then column. Holes, rotated
grids and custom origins are not supported. Reject self-intersecting or
self-touching boundaries rather than guessing their interior.

During preparation, calculate each retained sample at
`[x,y,terrain_elevation(x,y) + height_m]`. Receiver areas never seed or modify the
ground surface. Apply the same propagation to each generated point as an explicit
receiver; an area result is a spatial field, not the energy sum of its samples.
Different overlapping receiver areas remain distinct domains and retain their
own sample identities. An empty grid is valid and has an area summary with
`receiver_count: 0`.

Included building footprints are excluded automatically, independent of receiver
height. Additional `exclude_polygons` remain valid user-defined exclusions.
Submitting an acoustic building only as a mask is not an equivalent model.

Generated `receiver_id` is
`area:<parent-base64url>:<x-bits>:<y-bits>`, where parent-base64url is unpadded
base64url of the UTF-8 area ID and each coordinate is its calculated metric
IEEE-754 float64 bit pattern as 16 lowercase hexadecimal digits. Normalize
negative zero coordinates to positive zero. IDs do not depend on which other
samples were excluded. A collision with an explicit receiver ID is an input
error. Library (in-process) results retain `area_receiver_id`, `grid_column`,
`grid_row`, and XYZ for each sample. Over HTTP the samples are delivered as one
GeoTIFF per area instead; see [Area-receiver rasters](#area-receiver-rasters).

The GeoPackage adapter may preserve its existing UUIDv5 IDs instead: use the
parent object UUID as namespace and the name
`acalc-area-receiver-v1/<layer UUID>/<X bits>/<Y bits>`, as specified in the
model contract. The returned parent link and metric coordinates give the
adapter everything needed for this reversible identity mapping. These two
namespaces must not be mistaken for each other.

At most 10,000 area domains and 10,000 vertices per boundary/exclusion polygon
are accepted in the initial profile. Grid generation is cancellable, with at
most 1,000,000 bounding-box candidates
across all areas. Source–receiver pair products are not capped. Limits fail
with a diagnostic; do not enlarge spacing automatically,
truncate the grid, invent a fallback receiver or silently remove a region.

### Environment

| Object / field | Units | Default | Accepted initial profile |
| --- | --- | --- | --- |
| `atmosphere.temperature_c` | °C | `10` | −20 through 50 |
| `atmosphere.relative_humidity_percent` | % | `70` | 10 through 100 |
| `atmosphere.pressure_kpa` | kPa | `101.325` | Positive, at most 200 |
| `ground.factor` | Dimensionless G | `0` | 0 through 1 |
| `ground.elevation_m` | m | `0` | Uniform horizontal plane |

G is ground porosity, not a reflector's absorption coefficient. Atmospheric
calculation uses the exact base-10 octave centres specified by the 2024 method;
the input/output array labels remain the eight nominal frequencies.

### Terrain and ground regions

`terrain` is optional and contains `samples: [[x,y,ground_z], ...]` and
`breaklines: [[[x,y,ground_z], ...], ...]`, both defaulting to `[]`.
Every breakline has at least two
vertices. Building footprint and barrier base vertices also seed the surface.
Other object's ground vertices are normalized into `samples` by the frontend.
Acoustic coordinates never implicitly seed terrain. Conflicting coincident
samples or crossing breaklines with inconsistent elevations are invalid.

The engine builds a deterministic constrained triangulation and interpolates
linearly within triangles. Outside its convex hull, use the nearest boundary
point's interpolated elevation. One point defines a constant surface; collinear
points define piecewise-linear elevation along their axis, extended transversely
and clamped beyond its endpoints. With no explicit samples and no requested
inference use `ground.elevation_m`.
These coverage conventions are application rules, not surveyed terrain guarantees.

`ground.regions` is an ordered array of `{id, boundary, factor}`. Boundaries are
simple XY polygons (optional closing vertex, either winding, no holes) and G is
between 0 and 1. The last listed region wins at overlap and boundaries. Elsewhere
use `ground.factor`. Calculate source/middle/receiver factors by horizontal
length weighting on the complete screening-suppressed reference trace.

`settings.ground_method` selects:

- `general`: ISO 7.3.1 octave method and 2024 geometric correction; approximately
  planar ground, including constant slopes. D_omega is zero. The application
  also evaluates the user-selected method on irregular terrain as an unverified
  applicability extension; terrain planarity does not block a run. Terrain is
  retained, with no automatic method switch.
- `mean_path_height`: manually selected ISO 7.3.2 approximation. Selection declares
  non-pure-tone sound and porous/predominantly porous ground. G is not an arithmetic
  input to its formula. The assessment output is A-weighted; returned octave values
  use the same broadband ground correction and are labeled accordingly.

For the mean-height option, integrate signed reference-minus-terrain heights
exactly on each piecewise-linear profile. Divide the sum of horizontal profile
areas by the sum of ground-base endpoint chord lengths for the actual reference
legs. Terrain sample subdivisions do not create extra denominator legs. Preserve
negative heights in the hypothetical reference; physical routes still clear
terrain. Calculate A_gr and D_omega separately as specified in ALGORITHM.md;
include D_omega once and use raw A_gr in screening coupling. A zero denominator
is a degenerate-geometry diagnostic.

### Staged terrain construction and interpretation identity

The simple `terrain.samples`/`breaklines` arrays remain supported. Extended
terrain adds:

| Field | Type and meaning |
| --- | --- |
| `construction_profile` | Required when using `inference_points`, `breakline_definitions` or `ground_ref`. Use advertised `acalc_schema11` for the GeoPackage staged inference rules; record the implementation's immutable triangulation revision in results. |
| `inference_points` | Array of `{id, position_xy: [x,y]}` for original inferred ground vertices. Each ID is unique and position has no Z placeholder. Default `[]`. |
| `breakline_definitions` | Array of `{id, vertices: [GroundRef, ...]}` with at least two ordered vertices. Allows original terrain lines with inferred endpoints. Default `[]`. |

A breakline uses either the old all-explicit coordinate form or a referenced
definition; do not submit the same line twice. Mixed explicit/inferred lines
are permitted only if the backend advertises that combination. Explicit
segments whose two endpoints are known constrain the initial surface; no segment
may invent the missing endpoint of an inferred vertex.

`acalc_schema11` preparation is ordered:

1. Gather explicit `terrain.samples`, explicit breakline vertices, all literal
   `GroundRef.position` values, and literal building-footprint/barrier-base Z.
   These are ground, not roof, acoustic or reflector-bottom elevations. A
   standalone reflector may float above terrain and never seeds it implicitly.
2. Build the initial constrained surface from that complete explicit set.
   Deduplicate equal observations; reject conflicting coincident elevations or
   inconsistent crossing/on-line samples. For this profile the elevation
   consistency tolerance is `1e-7 m`, as in the model contract. Do not average
   conflicting survey values. Split consistent crossing breaklines.
3. Resolve **all** inference targets against that same initial surface. A target
   cannot seed its own surface or change the inferred value of a later target.
   Missing explicit samples are an error when inference is requested; the
   legacy plane setting is not a substitute.
4. Build the final surface from explicit and resolved original ground vertices
   and all resolved terrain-line constraints. Then resolve final-terrain
   locations and area-receiver samples. Resolve building roofs using the
   declared roof rule, retaining captured absolute roof elevations.

The single/collinear/hull-extension rules above apply to both stages. Vertical
terrain faces/overhangs are outside this 2.5D surface. No raster path, live raster
sampling or implicit raster-to-mesh construction is accepted. With **no ground
observations or inference request**, the legacy horizontal plane remains valid.

Sample ordering must not silently choose a different terrain diagonal between
backends. A construction profile must pin triangulation, degeneracy/tie rules,
consistency tolerances and extension behavior, and expose resolved vertices,
triangle indices and breakline edges through `prepared_geometry` in the result.
`acalc_schema11` identifies the model semantics; its triangulation revision and
cross-backend fixtures still need to be pinned by the implementations. A parser
or a generic claim to use Delaunay triangulation is insufficient evidence of
identical prepared terrain. Report an unsupported profile instead of selecting
another construction method.

### Ground-region policy and method applicability

The existing `ground.regions` form and its last-listed-wins behavior retain
that meaning for old requests. Extended requests may specify
`ground.region_policy`:

- `ordered_last_wins` (legacy default): later polygons win on overlapping
  interiors and boundaries. Array order is calculation input and provenance.
- `nonoverlapping_schema11`: reject interior overlap even at equal G; shared
  edges/points are allowed. Along a shared boundary use the mean of adjoining
  polygon G values; a lone polygon boundary uses its polygon's G. Uncovered
  ground uses the default. This is the policy required for equivalent QGIS
  schema-11 conversion. A zero-length endpoint region uses G at that endpoint;
  a vertically aligned source/receiver uses G at their shared XY.

Both policies require an explicit capability declaration; a backend supporting
only one must reject the other. Do not silently change a saved model's overlap
semantics to make it admissible. Ground regions never supply terrain elevations.

An optional top-level `applicability` object records the assessment declarations:

| Field | Meaning |
| --- | --- |
| `sound_character` | `not_pure_tone`, `pure_tone`, or `unknown`. |
| `ground_character` | `porous_or_predominantly_porous`, `other`, or `unknown`. |
| `assessment_quantity` | `a_weighted` or `octave`. |
| `evidence` | Text identifying the basis for the declarations, not a conformity claim. |

For extended `mean_path_height` requests require `not_pure_tone`,
`porous_or_predominantly_porous` and `a_weighted`; unknowns do not establish
applicability. Legacy selection alone retains its documented declaration
semantics. Do not invent a numerical G threshold for "predominantly porous" or
switch the ground method automatically. General-mode and long-term applicability on irregular terrain remain
unverified engineering extensions, recorded in result limitations rather than
rejected by a planarity gate.

### Reflectors and barriers

`reflectors` contain finite, vertical, two-sided opaque rectangles. Fields are
`id`, `start`, `end`, `height_m`, and optional `absorption`. Start/end are bottom
corners at equal Z; height is positive vertical extent. `absorption` is exactly
eight energy-absorption coefficients in [0,1]. Omitted or whole-field null means
0.1 in every band; individual null coefficients are invalid. Coefficient 1
means no reflected energy in that band. Default absorption follows the facade
convention; applying it to every supplied rectangular face is an explicit
application convention. The input declares that the face is closed and opaque.

`barriers` accept either the legacy `start`, `end`, `height_m` rectangle or
`vertices` (ground XYZ polyline) with `heights_m` per vertex. Polyline tops
interpolate base Z plus height. Preserve a simple open polyline: no self-crossing,
retracing or branching. `surface_density_kg_m2 >= 10` and `closed: true` declare
the opaque-screen applicability assumptions. Barriers screen but do not reflect;
reflective surfaces use `reflectors` without duplicating a screen in both arrays.

`buildings` contain `id`, `footprint` (ground XYZ polygon), and either uniform
`height_m` above each base vertex or absolute `roof_elevations_m` per vertex.
Supply optional octave `absorption`, otherwise 0.1. Footprints may be concave;
roof elevations must define a plane in the current geometry profile. The solid
provides roof/facade shielding and facade reflections; roof reflections and
nonplanar roof meshes are not currently supported. Included footprint vertices
seed terrain and footprints exclude area-receiver samples.

Obstacle IDs must be unique across buildings, reflectors and barriers. The
finite-face geometry and frequency checks remain mandatory. A proven infeasible
route is discarded independently; numerical failure does not imply silence.
Combined routes use the algorithm's single unfolded vertical calculation or
per-reflection mixed-section losses and total-contribution cap. Supported
extended-source obstacle combinations remain subject to conservative transition
classification; sampling agreement alone cannot establish clear visibility.

### Extended solids, wall roles and face identity

The preceding forms are the legacy geometry profile. The following additive
forms require the corresponding advertised capabilities; they do not turn a
limited rectangular/planar implementation into a general solid solver.

An extended building has `id`, optional `absorption` with the same coefficient
rules as above, exactly one base representation, and exactly one roof
representation:

| Field | Meaning |
| --- | --- |
| `footprint` | Existing ground XYZ perimeter; simple XY polygon, no holes. |
| `ground_vertices` | Alternative array of `GroundRef` objects in authored perimeter order, at least three unique XY vertices. |
| `height_m` / `roof_elevations_m` | Existing roof representations; mutually exclusive with `roof`. |
| `roof` | Object with `mode` and the mode-dependent data below. |
| `roof_triangles` | Optional explicit triples of perimeter indices defining the roof surface. Required for a nonplanar roof; requires `building_roof_mesh`. |
| `reflecting_faces` | Optional array of stable face IDs; omission enables all exterior faces (facades, planar roofs and roof facets) in Rust; Go currently defaults to facades only. An explicit list selects exactly those faces and `[]` disables building reflections. The solid continues to screen. Roof participation requires advertised `roof_reflections`. |
| `face_absorption` | Optional object mapping face IDs to eight energy-absorption coefficients. Overrides the building material for those faces; unknown faces are invalid. |

`roof.mode` preserves the six model meanings:

| Mode | Required data | Resulting absolute roof Z |
| --- | --- | --- |
| `average` | `height_m` nonnegative | Mean resolved perimeter ground Z + height. |
| `highest` | `height_m` nonnegative | Maximum resolved perimeter ground Z + height. |
| `lowest` | `height_m` nonnegative | Minimum resolved perimeter ground Z + height. |
| `absolute` | `elevation_m` finite | The same absolute elevation at every vertex. |
| `individual` | `heights_m` array of nonnegative per-vertex heights | Resolved ground Z + corresponding height. |
| `absolute_individual` | `elevations_m` array of finite absolute elevations | Captured authored ground Z + M, **before** any ground inference. |

Mode-dependent arrays have one value per perimeter vertex, excluding a repeated
closing vertex. Do not include fields from other modes. Negative absolute
elevations are valid; a roof below its resolved base or a degenerate solid is
invalid. Roof triangles cover exactly the footprint without overlapping
interiors or invented diagonals. Facades join corresponding ground/roof edges;
roof and facade surfaces form one opaque building, not independent screens.
Buildings and barriers are ground-anchored: prepared ground bounds their lower
occupied volume. Authored base elevations do not create an acoustic underpass.
Floating finite surfaces use the reflector form, under its separate profile.

Preserve authored vertex order and identities. An optional repeated closing
vertex is removed before indexing; never rotate, sort or reverse the submitted
index list without also updating every reference. `wall:a:b` identifies a
perimeter edge directed counterclockwise around the solid (reverse the edge
indices for a clockwise stored ring), `roof` a planar roof, and `roof:a:b:c` a mesh facet using its
sorted vertex indices. Determine outward normals from the solid, independent
of input winding. For QGIS attachments, preserve its CCW authored indices and
first-ear roof connectivity described in GEOPACKAGE.md. Do not regenerate
different diagonals on the service. Expose resolved face IDs, polygon vertices
and outward normals in preparation diagnostics.

Extended barriers preserve the legacy screen-only default and add:

| Field | Meaning |
| --- | --- |
| `ground_vertices` | Alternative to `vertices` or legacy `start`/`end`: ordered `GroundRef` base polyline. |
| `top_elevations_m` | Alternative to `heights_m`: finite absolute top Z at each polyline vertex. Tops must be above their bases. |
| `role` | `screen` (default) or `screen_and_reflector`; the latter requires `reflecting_barriers`. |
| `opacity` | Alternative to legacy `surface_density_kg_m2` plus `closed`: `{ "kind": "declared_opaque", "basis": "..." }`, with a nonempty explanation of the opaque closed-surface assumption. Requires profile support. |
| `absorption` | Eight material coefficients for reflecting walls, same default/rules as reflectors. |
| `segment_absorption` | Optional array with one eight-band material per base segment, replacing the uniform material on those segments. |

Use exactly one base form and one corresponding top form. The legacy rectangle
uses `height_m`; polyline forms use `heights_m` or `top_elevations_m`. Do not
provide both opacity declarations or fabricate a measured density for a model
that only declares an opaque wall. Segment `i` joins vertices `i` and `i+1`,
with stable face ID `segment:i`. Wall faces remain two-sided. The same wall
participates once in screening and, if requested, reflection; do not duplicate
it in `reflectors`. Ground vertices seed terrain, tops do not. Bent/sloping
faces and their contacts need a supported propagation profile, not just parser
support. Materials remain validated even if reflection order is zero.

For general `planar_reflectors`, a reflector may supply `vertices` (three or
more coplanar absolute XYZ boundary points) instead of `start`/`end`/`height_m`.
The polygon must be simple, finite and nondegenerate; concavity is permitted,
holes are not. The plane's normal follows vertex winding. Optional `side` is
`both` (default), `front` or `back`; front is the positive-normal half-space.
Side restricts reflection only; the opaque face still screens from either side.
The complete finite face, its material and size eligibility are used for every
reflection. These acoustic surface vertices do not become terrain supports.
Nonvertical geometry and nonvertical long-term meteorology are separately
advertised capabilities. No terrain, building or barrier may be replaced by
unrelated reflecting faces when doing so loses its screening/solid semantics.

### Building-dependent source placement

All attached sources are entries in `sources`, with their own physical source
ID and ordinary power/directivity data. They reference an included building;
they are not an extra implicit emitter inside `buildings`.

For already resolved geometry, `host_building_id` and optional `host_face_id`
identify the supporting solid/face. A supplied face must exist and agree with
the geometry. Otherwise resolve one unambiguous supporting face as described
above. A source spanning different face planes is invalid as one attached area.
The QGIS adapter must validate stored `face_sources.vertex_count` and face IDs
against the current authored perimeter before conversion; dropping that check
can silently rebind a stale attachment to a different edge or roof facet.

For geometry that depends on terrain/roof preparation, a point or area source
may instead supply `host_building_id`, required `host_face_id`, and `attachment`:

| Field inside `attachment` | Meaning |
| --- | --- |
| `placement_profile` | Required advertised, versioned placement convention; `acalc_schema11` means the frames and containment rules in the model contract. |
| `frame` | Roof only: `building_local`, `roof_top_edge`, or explicit `legacy_world`. `legacy_world` maps stored records with no frame. Wall attachments omit it and use the fixed wall frame. |
| `x_m`, `y_m` | Required finite nonnegative offsets in the selected face's right/down frame. |
| `mounting` | Point only, required: `surface` or `standoff`. Area attachments omit it; rectangles are always on the surface. |
| `distance_m` | Point only. `surface`: omitted or exactly 0. `standoff`: required outward normal displacement of at least 0.05 m. |
| `width_m`, `height_m` | Area only: required finite positive rectangle dimensions in the right/down frame. |

`attachment` excludes every absolute, terrain-relative or indexed geometry
field for that source. An area zone may use its own `attachment`, inheriting
the parent's host/face; all zones remain on that one face plane and must not
overlap. The parent then has `zones` and no `attachment` or other geometry.
Line sources can use resolved/indexed geometry plus host identity; rectangular
attachment placement does not define a line.

For walls, origin is the roof at vertex a of `wall:a:b`, right follows the
horizontal edge a-to-b, and down is vertical. Roof placement uses the exact
model-contract frame, top-edge selection, tie rules and authored facet identity;
record its implementation revision. A point anchor and the whole area rectangle
must lie inside/on the selected face, including concave boundaries. Resolve
after terrain and roof preparation. A point's displacement follows the host
outward normal; do not add another offset automatically. A rectangle becomes
two triangles with actual area `width_m * height_m`, retaining its source ID.

Attachments sample final terrain for physical source height and never seed it.
A `surface` point is an opening, louvre or panel on the face: it is
host-attached, receives the outward launch restriction and is never reflected
by its host. A `standoff` point is a unit mounted clear of the face: an
ordinary point source with no host launch restriction, reflected by the host
like any other face. The 0.05 m minimum is Acalc engineering policy, not an ISO
value; it keeps the host image source distinct from the source. Independently
of attachment, any point source 1e-7 m to 0.05 m from a building face whose
polygon it projects onto is rejected as invalid input, so near-contact cannot
silently select either convention. Placement profile revision
`iso-rust-face-placement-v3` requires `mounting`; v2 inferred the convention
from distance.
No attachment field requests transmission, an extra facade gain or a different
emission normalization. Supplied emission describes exterior radiation under
ALGORITHM.md's host convention.

### Foliage, site and housing regions

`misc_regions` carries effects contributing to `A_misc`. Every region has a
unique `id`, `kind`, `method_profile`, and method-dependent data. Reject unknown
kinds or profiles. A profile names an immutable method/edition, path-tracing
rule, required data schema, units, overlap rule and applicability restrictions;
its declaration is part of `/v1/capabilities`. A bag of unchecked parameters
is not an implemented capability.

The defined simple foliage form is:

```json
{
  "id": "woodland-1",
  "kind": "foliage",
  "method_profile": "iso9613_2024_foliage_simplified_acalc_v1",
  "boundary": [[20,-10],[60,-10],[60,10],[20,10]],
  "canopy_height_m": 10
}
```

`boundary` is a simple XY polygon with at least three vertices, either winding,
optional closing vertex, no holes. `canopy_height_m` is required finite positive
height above final terrain. The volume is dense vegetation from terrain to that
height. It does not seed terrain or change G. Overlapping foliage volumes are
united; a traversed portion is counted once. Empty `misc_regions` means no
foliage; omission of height never selects an assumed 10 m forest.

This profile selects ISO 9613-2:2024 A.2.2, not the detailed forest model.
Its application path convention is the existing
Trees solver contract, specifically its arc/ray
geometry, table/20 m boundary convention, route intersections, overlap union
and unsupported geometries. Its older cap-normalization rule is superseded
here by ALGORITHM.md's completed-contribution cap.
Record those application conventions and their verification gaps. Evaluate
foliage on physical routes and on the unscreened cap reference according to
that profile; never substitute straight-line distance through an XY footprint
for the defined 3D foliage traversal. Refinement must account for foliage
boundaries across an extended source.

`industrial_site`, `housing`, and detailed foliage are reserved extension kinds:
ALGORITHM.md identifies the term but does **not** completely specify these
methods. Before advertising one, both backends need an agreed profile defining
its typed input and computation. At minimum retain the actual region geometry,
vertical extent relative to terrain, applicable octave attenuation data and
their units/reference, plus method-specific descriptors (for example building
coverage/heights/open corridors for housing, or forest structure for a detailed
foliage method). Those examples are requirements to resolve, not default values
or an executable invented formula. A profile's JSON Schema must specify all
required descriptors, their validation and path/overlap rules. Selected model
effects without such a profile are rejected.

An aggregate attenuation profile must identify which explicit objects it
represents, through `represented_object_ids`, and forbid double-counting their
loss. Generic site attenuation does not enable the special long-term industrial
screening treatment of ISO 7.4.1 Note 1, which ALGORITHM.md explicitly excludes.
No effect defaults to an arbitrary per-metre loss or cap merely to accept it.

### Settings

Settings use the defaults below; `long_term` requires an explicit `c0`.
Resolved values are echoed in the result.

| Field | Default | Meaning / accepted values |
| --- | --- | --- |
| `ground_method` | `"general"` | `"general"` or manually selected `"mean_path_height"`; see ground methods below. |
| `max_reflect_order` | `0` | Integer 0–5; counts reflecting faces only. |
| `pruning_strategy` | `null` | `"dba"` or `"dbc"`: automatic live pruning that protects that weighted level; `null` disables it. Must be one of the advertised `pruning_strategies`. |
| `minimum_contribution_db` | `null` | Optional fixed absolute unweighted octave SPL threshold in dB re 20 µPa. Omitted or `null` disables it; any finite number enables it. Equality is retained. Must be null with a `pruning_strategy`. |
| `area_raster_factor` | `0.5` | Positive, at most 0.5; cell extent/distance refinement factor. |
| `area_max_extent_m` | `null` | Optional positive maximum cell extent. |
| `area_relative_tolerance` | `0.001` | Positive local coarse/fine energy discrepancy ratio, at most 1. |
| `area_absolute_tolerance` | `1e-12` | Nonnegative received-energy discrepancy allowance, allocated by cell/source measure (length or area). Energy is relative mean-square pressure, not dB. |
| `area_max_depth` | `16` | Integer 1–24. At the limit, unresolved coverage keeps its trial energy and conservative bound; the job fails unless that bound fits the source-receiver tolerance. |
| `meteorological_mode` | `"downwind"` | `"downwind"` or `"long_term"`, subject to supported source/path combinations. |
| `c0` | `0` | Finite nonnegative C₀ in dB; must be zero for downwind output. |

With a `pruning_strategy`, the engine evaluates each reflection order of each
source as a separate task. Direct paths are never pruned. Once every source's
direct paths have completed, each reflection task uses, when it starts,
per-band cutoffs of
(quietest receiver's weighted level from contributions completed so far)
− (10 log10(source count) + 15 dB for dBA or + 20 dB for dBC)
− (band weighting). No order waits for the previous one; cutoffs only rise as
tasks complete. Pruned results therefore depend slightly on scheduling and can
differ between runs; unpruned results do not. `diagnostics.pruning.stages`
reports, per reflection order, `tasks_without_cutoff` and the lowest and highest
applied `weighted_cutoff_db` (null if none). The margin is empirical: weighted
totals stayed within about 0.02 dB of unpruned results in the recorded tests,
but no bound is certified and individual octave bands can lose more. Path
counts include beams re-expanded for each order.

Either kind of cutoff prunes a band only when a conservative upper bound on its
energy is below the cutoff, before its paths are built. Reflection beams are
not extended when no band can reach the cutoff through them
(`path_search.prefix_pruned_beams`). Point families with only vertical
reflectors use the unscreened family cap times source power; line/area
families sum per-cell bounds over all zones and skip integration of pruned
bands. Computed contributions are never discarded. Equality survives. Pruned
receiver connections report their bound as omitted energy; prefix-pruned beams
report nothing. See ALGORITHM.md, *Pruning and omission accounting*.

The retired `receiver_margin_db` input is accepted only for compatibility
(null or a finite positive number), ignored, and omitted from resolved result
settings. The plugin removes saved values when loading a project and no longer
sends this key. Historical requests remain replayable without relative pruning.

For implementations that use reflector-prefix pruning, interim pseudo-receptor
bounce positions are internal pruning geometry only. They may differ by band
and diffraction alternative and must not appear as physical result paths or
additional receiver contributions. On completing a connection to a real
receiver, resolve every bounce to its final position and recalculate the full
attenuation ledger. Preserve selected faces/order, not interim coordinates.
Interim pruning replaces net ground attenuation `A_gr - D_omega` with one
-6 dB allowance and must not add D_omega again. An unverified minimum among
sampled face points is not a certified pruning bound; the evaluate-all fallback still applies. This rule adds no request field
and does not advertise prefix pruning as implemented by `iso_go` 0.4.0.

For each source position, receiver and ordered reflection sequence, calculate
one ground context on the screening-suppressed reference path. All vertical,
lateral and mixed alternatives and the cap reference share its `A_gr` and
`D_omega`. Diffraction detours and adjusted contacts do not move the ground
sampling trace. This follows the latest ALGORITHM.md and supersedes the earlier
completed-detour ground convention.

Sum completed route energies then cap each band at its unscreened reference.
The reference retains material, finite-face/size eligibility and the shared
ground context, while suppressing screening and the host emission-side mask.
Physical routes retain the host launch mask. A proven ineligible reference has
zero energy; numerical failure cannot establish zero. Apply this model cap
before cutoffs and totals, per family. Extended-source point transfers are capped
before quadrature; cutoffs remain at whole-source/family scope. Cap reductions
are model treatment, not pruning omissions.

The absolute test applies to each complete point/path family, or the complete
original line/area-source/reflection family summed over all its zones. Never apply it to
an artificial quadrature sample or individual integration triangle. Removed
energy contributes zero to the retained result, not a numeric 0 dB sample.
Many individually subthreshold contributions can have a meaningful sum.

The Go numerical resource profile accepts at most 10,000 initial triangles
per source, 10,000 live area-integration
cells, and 1,000,000 candidate grid
points across all area receivers. Exceeding a live-cell limit fails immediately
rather than waiting for a worker slot. The initial profile also limits coordinates to
±10⁹ m, and finite source levels from −200 to 300 dB. Positive areas must exceed
10⁻¹² m². These are declared numerical/resource restrictions, not ISO limits.
Neither implementation imposes a source/receiver pair-count or reflection-family-count limit.
Family enumeration remains cancellable; integer overflow is a numerical failure.
Geometric/numerical precision exhaustion, overflow and positive-transfer
underflow must produce diagnostics rather than false silence or convergence.

### Requested diagnostics and a combined input example

Optional top-level `output` controls additional data, never the acoustics:

| Field | Default | Meaning |
| --- | --- | --- |
| `prepared_geometry` | `false` | Return resolved terrain, geometry and source descriptors; requires `prepared_geometry`. |
| `path_diagnostics` | `"none"` | `none`, `summary`, or `full`; non-default values require `path_diagnostics`. Summary returns family totals/eligibility; full additionally returns route/sample ledgers. |
| `integration_reports` | `"summary"` | `summary` or `full`; requires feature `integration_reports`. Extended-source zone integrations are always aggregated in `diagnostics.integration_summary`. `full` also lists one report per zone, receiver and reflection family in `diagnostics.integration` (about 1 KB each). That list grows with the number of integrations, not the number of receivers: the order-1 road model gave 19,067 (19 MB), and an order-3 model can reach hundreds of gigabytes. Request it only for a specific run that needs it for compliance evidence or debugging. |
| `area_source_contributions` | `false` | Generated area-grid receivers report totals only; set `true` to add their per-source `sources` breakdown. Requires feature `area_receiver_totals`. Library API only: the HTTP API rejects `true` with `422 unsupported`, because its area samples are raster pixels. Explicit point receivers always retain their source contributions. Receiver totals, omission diagnostics and calculation order do not depend on this option. |

The backend advertises diagnostic size limits. It must reject or fail a request
it cannot fulfill, rather than silently truncate a requested ledger. Production
totals and deterministic reduction are unchanged by diagnostics. Retain the
submitted snapshot even when diagnostics are disabled.

This complete **schema illustration** combines inferred source/building ground,
heterogeneous line zones, a roof-dependent facade emitter, a reflecting wall,
mixed ground, foliage and a receiver grid. It supplies no acoustic reference
results and is not a promise that either backend currently accepts all these
combinations. Submit it only to a profile advertising all the required features.

```json
{
  "terrain": {
    "construction_profile": "acalc_schema11",
    "samples": [[-10,-20,0],[100,-20,0],[100,50,0],[-10,50,0]],
    "inference_points": [
      {"id":"line-a","position_xy":[0,20]},
      {"id":"line-b","position_xy":[10,20]},
      {"id":"line-c","position_xy":[20,20]},
      {"id":"building-a","position_xy":[0,0]},
      {"id":"building-b","position_xy":[10,0]},
      {"id":"building-c","position_xy":[10,10]},
      {"id":"building-d","position_xy":[0,10]}
    ]
  },
  "sources": [
    {
      "id":"pipe","kind":"line","power_basis":"power_per_m",
      "zones":[
        {
          "id":"section-a","spectrum":[70,71,72,73,74,73,72,71],
          "geometry":{"vertices":[
            {"ground_ref":{"inference_id":"line-a"},"height_m":2},
            {"ground_ref":{"inference_id":"line-b"},"height_m":2}
          ]}
        },
        {
          "id":"section-b","spectrum":[65,66,67,68,69,68,67,66],
          "geometry":{"vertices":[
            {"ground_ref":{"inference_id":"line-b"},"height_m":2},
            {"ground_ref":{"inference_id":"line-c"},"height_m":2}
          ]}
        }
      ]
    },
    {
      "id":"facade-panel","kind":"area",
      "host_building_id":"hall","host_face_id":"wall:0:1",
      "power_basis":"total_power","spectrum":[80,80,80,80,80,80,80,80],
      "attachment":{
        "placement_profile":"acalc_schema11",
        "x_m":2,"y_m":1,"width_m":2,"height_m":1
      }
    }
  ],
  "buildings":[{
    "id":"hall",
    "ground_vertices":[
      {"inference_id":"building-a"},{"inference_id":"building-b"},
      {"inference_id":"building-c"},{"inference_id":"building-d"}
    ],
    "roof":{"mode":"average","height_m":6},
    "absorption":[0.1,0.1,0.1,0.1,0.1,0.1,0.1,0.1]
  }],
  "barriers":[{
    "id":"yard-wall","vertices":[[50,-5,0],[50,35,0]],
    "heights_m":[3,3],"role":"screen_and_reflector",
    "opacity":{"kind":"declared_opaque","basis":"Closed opaque wall in the model"},
    "absorption":[0.2,0.2,0.2,0.2,0.2,0.2,0.2,0.2]
  }],
  "misc_regions":[{
    "id":"trees","kind":"foliage",
    "method_profile":"iso9613_2024_foliage_simplified_acalc_v1",
    "boundary":[[25,-10],[35,-10],[35,10],[25,10]],"canopy_height_m":8
  }],
  "ground":{
    "factor":0,"region_policy":"nonoverlapping_schema11",
    "regions":[{"id":"grass","boundary":[[15,-15],[90,-15],[90,40],[15,40]],"factor":1}]
  },
  "receivers":[{"id":"receiver-1","position_xy":[80,20],"height_m":1.5}],
  "area_receivers":[{
    "id":"yard-grid","boundary":[[60,0],[80,0],[80,20],[60,20]],
    "height_m":1.5,"spacing_m":10
  }],
  "atmosphere":{"temperature_c":10,"relative_humidity_percent":70,"pressure_kpa":101.325},
  "settings":{
    "ground_method":"general","meteorological_mode":"downwind","c0":0,
    "max_reflect_order":1,"minimum_contribution_db":null
  },
  "output":{"prepared_geometry":true,"path_diagnostics":"none"}
}
```

## Job states and snapshots

Job states, cancellation (`progress.phase: "cancelling"` until engine-iso has
stopped), snapshots, `sequence`, `finished_at`, `error` and `artifacts` are
acalc-engine's; see its API.md. Jobs start `running`. engine-iso supplies the
snapshot's `progress` object: `total_task_points`, `completed_task_points`,
`percent`, `phase` (`preparation`, `propagation`, then `completed`; display
metadata only, clients use `state` for the lifecycle), `path_counts` and
`model_summary` (below).

Go also publishes an optional `progress.work` object:

```json
{"workers": 8, "active_workers": 8, "queued_jobs": 1250, "completed_jobs": 32000, "operations": 850}
```

All values are nonnegative integers; `workers` is positive and `active_workers`
cannot exceed it. `queued_jobs` and `active_workers` are instantaneous gauges.
`completed_jobs` counts finished queue jobs, including searches, continuations,
integration and screening. `operations` counts sampled/accounted evaluations;
neither counter is an acoustic path total or a predicted total. The queue may
grow as work discovers more work. Do not derive a percentage from these values.

Go emits fresh solver snapshots every 500 ms while running, including during
preparation and lengthy receiver/integration work before a beam completes.
Milestone callbacks also publish immediately. Polling and SSE expose these
activity changes with a new sequence even when task points and `path_counts`
are unchanged. This is a reporting cadence, not a real-time delivery guarantee.
The frontend displays worker/queue activity when supplied, clears it between
variants and hides it after termination; APIs without `work` remain supported.
Callbacks are serialized, and the periodic reporter is joined before calculation
return so late updates cannot overwrite a terminal state.

### Distribution statistics (farm calculations)

A calculation that acalc-engine spreads over a farm has a `distribution` object
in its snapshots; acalc-engine's API.md defines it. It never affects the result.

### Progress calculation

After receiver expansion, progress snapshots may include `model_summary` with
integer `source_count` and `receiver_count` (including generated grid receivers
after exclusions). Their product is the number of source–receiver pairs. Both
implementations publish these counts after preparation and retain them through
completion. Until then, the frontend distinguishes explicit receiver counts from
unresolved receiver areas. Display model counts, phase, percentage and elapsed
time to users; task-point values below are transport bookkeeping only.

The Rust engine publishes `path_counts: {"checked": 0, "pruned": 0}` from
submission through terminal state, with monotonically increasing integers. They
count calculations: discovery goes source → stub/reflection beam → spatial
receiver query, and each admitted receiver hit (including each area-grid point)
creates one terminal calculation stub with its own geometry, energy and pruning
decision. `checked` counts these stubs and `pruned` those that kept no positive
energy and have positive omitted energy, so `pruned` never exceeds `checked`.
Octave bands and line/area quadrature samples are not counted separately. Every
stub lies in exactly one work item, so the counts do not depend on how the work
is divided, on worker counts or on nodes, and the final values equal
`diagnostics.path_search.receiver_stub_counts`. (Until 2026-10-14 `checked`
counted stubs/beams once and `pruned` counted beams pruned at every connected
receiver of one receiver block; the retired Go backend counted beams.)
`diagnostics.path_search` separately reports receiver candidates, confirmed
receiver connections, empty beams, exact geometry rejections, reflector
candidates and bands skipped using point-family energy bounds. Children share
the immutable reflection history and never extend to reflectors. An empty beam
is one whose volume contains no receiver, before exact geometry tests.

The beam search defines the same work unit in both backends, but finite-geometry
and eligibility profiles can produce different counts. Go's exact connection
count precedes octave eligibility; Rust's also applies its band mask. Only Rust
currently uses `bounded_pruned_bands` to count screening work avoided by energy
bounds. Go retains its completed-contribution cutoff, which Rust no longer applies. Go task-point
credit advances over each source's completed reflection-order stages; this is
work progress, not a runtime prediction.
The final counters are also retained in result `diagnostics.path_counts`.
The frontend displays them during the run and in saved Run details. Services
without this optional progress extension display no fabricated counts.

```text
total_task_points = 1 + point_source_count + 2 * line_source_count + 3 * area_source_count
percent = 100 * completed_task_points / total_task_points
```

Freeze the denominator when the job is accepted. Preparation receives 1 point,
each original point source 1, each original line source 2, and each original
area source 3. `iso_go` advertises `line_source: 2` in `progress_task_points`. Receiver areas receive no additional task points.
A source's points are divided equally over all explicit and generated point receivers; each completed receiver
operation earns that share once all required paths, integration and its local
reduction succeed. Fractional completed points are permitted. For example, one
point plus one area source gives five points: preparation 20%, the point source
20%, the area source 60%, regardless of their triangle or reflection counts.

Refinement, reflection enumeration and internal samples never increase the
denominator. Failed/cancelled unfinished shares earn no points. Progress is
monotone; elapsed time never generates artificial points. Hold the final share
until final reduction succeeds and the result is published atomically with
`state: "completed"`, `percent: 100` and completed points equal to total points.
If every defined receiver area is empty and there are no explicit receivers,
complete each source's empty receiver set once and return empty receiver results.
Only a completed job can report 100%. Frontends should truncate/cap a running
integer progress display at 99 instead of rounding a sub-100 value to 100.

### Server-sent events

acalc-engine's events route sends the current snapshot at once, the latest
every 500 ms while it changes and the terminal snapshot promptly, then closes
(see its API.md). There is no historical replay; recover a lost connection by
polling status and reconnecting. Do not submit a new job merely because its
event stream disconnected.

## Completed results

The result is immutable and available only in the `completed` state. Otherwise
the results route returns HTTP 409. Failed/cancelled jobs expose diagnostics,
not partially calculated receiver levels.

Top-level fields:

| Field | Type / meaning |
| --- | --- |
| `method` | String identifying both propagation and atmospheric editions. |
| `profile` | String describing the actual supported calculation profile. |
| `atmosphere`, `ground`, `settings` | Fully resolved input values/defaults. |
| `receivers` | Explicit receiver results in request order. (Library API: followed by generated points in area-definition order and then row-major order.) |
| `area_receivers` | One summary per requested area: `area_receiver_id`, `receiver_count`, `spacing_m`, `height_m`, `grid` (`origin` `[xmin,ymin]` metres, `columns`, `rows`), and for submitted calculations `raster` and `sample_warnings` (below). Empty areas remain represented. |
| `limitations` | Array of numerical, capability and verification qualifications. |
| `diagnostics` | Ground method, broadband octave approximation flag, assessment quantity, integration convergence, omission basis and count of unbounded prefix cuts. |
| `operations` | Number of internal evaluated/accounted operations. Diagnostic only, with no operation-count ceiling; not task-point progress, and need not agree across languages. |

Each receiver has `receiver_id`, `position` (absolute metre XYZ), `sources` in
request order, and the level/omission fields below. In library results, generated
points additionally have `area_receiver_id`, `grid_column`, and `grid_row`
(zero-based bounding-box indices); explicit points omit these three fields. Each
source contribution has `source_id` and the same fields, without a nested
`sources` array.

Library generated area points omit `sources` unless
`output.area_source_contributions` is `true`; they always include all
receiver-level octave, dBA, state and omission fields. The engine accumulates
area totals directly and does not retain per-source area results unless detail
is requested. The QGIS frontend sends `false` explicitly when
`area_receiver_totals` is advertised. Point receiver results continue to require
the complete source breakdown.

### Area-receiver rasters

Feature `area_receiver_rasters` (API 1.2). In submitted calculations, generated
area samples are removed from `receivers` and written to one GeoTIFF per area,
which engine-iso hands to acalc-engine as a job artifact, held in memory with
the job and evicted with it. Large grids therefore never inflate the result
document; clients download each raster separately after reading the results.
Each area summary carries:

| `raster` field | Meaning |
| --- | --- |
| `href` | `GET` link for the artifact (also listed in the completed snapshot's `artifacts`). |
| `artifact`, `media_type`, `format` | Engine-generated name (`area-<n>.tif`), `image/tiff`, `GeoTIFF`. |
| `bytes`, `sha256` | Exact size and lowercase hex SHA-256 of the downloaded file. |
| `columns`, `rows` | The area's full bounding-box grid, as in `grid`. |
| `data_type`, `bands`, `band_units` | `Float32`; nine bands `dBA`, `63 Hz` … `8000 Hz`; `dBA` then `dB`. |
| `nodata` | `-3.4028234663852886e38` (Float32 lowest). |
| `geotransform` | GDAL order `[xmin−s/2, s, 0, ymin+(rows−½)s, 0, −s]`; pixels are centred on samples, north-up. |
| `coordinates` | Request Cartesian metres. The file has no CRS; the client assigns its own. |

Pixel `(column, rows−1−grid_row)` holds sample `(grid_column, grid_row)`: band 1
its `a_weighted_db` and bands 2–9 its `octave_db`, rounded to Float32. NoData
means no sample (outside the boundary, excluded or inside a building) **or** a
null level (zero calculated energy) in that band; nothing is interpolated.
Per-sample band states, omission fields, source contributions and warnings are
not delivered. Instead `sample_warnings` digests the area's pathway warnings:
`samples_with_warnings`, `incomplete_samples`, `bounded_samples`,
`failed_pathways`, `warning_counts` (by `kind`) and up to eight `examples`
(complete warning objects with distinct messages). The run-level
`warning_summary` still counts every sample.

The artifact route returns `409 results_unavailable` before completion and
`404 not_found` for an unknown job or name. The layout is an uncompressed,
little-endian classic TIFF with separate band planes in strips, readable by
GDAL; clients should verify `bytes` and `sha256` before use.

| Field | Meaning |
| --- | --- |
| `octave_db` | Eight finite levels or nulls, dB SPL re 20 µPa. |
| `band_states` | Eight strings: `calculated`, `cutoff_limited`, or `exact_zero`. |
| `a_weighted_db` | Finite broadband dBA or null when no energy is retained. |
| `state` | `calculated` if any band retains energy; otherwise `cutoff_limited` if any energy was omitted, else `exact_zero`. |
| `omitted_energy` | Eight nonnegative relative mean-square-pressure values omitted within the evaluated source/path universe. |
| `omission_increase_db` | Eight finite increases or nulls, as defined below. |
| `omission_upper_db` | Absolute levels of retained plus numerically omitted energy, including when retained energy is zero; null when nothing was omitted. |

For retained energy E > 0, the band level is `10 log10(E)`. A calculated band
can still have positive omitted energy; inspect the omission fields. Null plus
`cutoff_limited` means no retained energy but positive omitted energy. Null plus
`exact_zero` means zero energy within the declared method/profile and selected
reflection order, not measured physical silence. Never display a null as 0 dB.

For E > 0 and reported omitted energy O, the omission increase is
`10 log10(1 + O/E)`. For E = O = 0, it is numeric zero. For E = 0 and O > 0, it
is null because no finite relative increase exists; the absolute omitted upper
level can be derived as `10 log10(O)`. Initial Go O sums evaluated families
removed by the completed absolute cutoff. Rust O adds computed completed-family
omissions and conservative point-family cap upper energies for skipped screening
bands; it may overstate actual missing energy. Neither is an uncertainty interval for
area quadrature or unenumerated reflection orders. No unbounded prefix cuts are
performed in that profile.

Apply A-weighting once when summing receiver octave energies; never sum dB
values directly. Source totals sum incoherently, in stable order independent
of task completion. Ordinary floating-point tolerances may be necessary across
language implementations; agreement is tested against independent evidence,
not a promise of identical internal operation counts or progress timing.

### Extended result identity and prepared geometry

Extended profiles additionally return `api_version`, `implementation` (name,
version and immutable build revision), `algorithm_revision`, `profile_id`, and
`component_profiles` (the actual IDs/revisions used). Echo the effective
`applicability` and every resolved setting. Record the chosen ground-region
policy, terrain construction revision, directivity conventions and foliage
profile; a method-edition string alone cannot identify this calculation.

`prepared_geometry`, when requested, contains the following fields. Empty
collections remain empty rather than disappearing into an implicit fallback:

| Field | Contents |
| --- | --- |
| `terrain` | `construction_profile`, `revision`, `vertices` (ground XYZ), `triangles` (index triples), `breakline_edges` (index pairs), `inferred_points` (`id`, resolved `position`) and `coverage_mode` (`plane`, `single_point`, `collinear`, or `triangulated`). Include plane elevation or the ordered collinear support indices for the degenerate forms. |
| `sources` | One entry per original `source_id`: kind, resolved absolute point/polyline/mesh, zones with `zone_id`, actual `measure`/`measure_unit` (`m` or `m2`), normalized total octave power, and resolved host/face identity. |
| `source_descriptors` | Per `source_id`: fixed `centre` XYZ, `height_m`, `height_basis` (`actual_constant`, `predominant`, or `not_required`), and centroid weighting convention. A wholly silent source may use null centre/height with `not_required`. For downwind or other nonapplicable Cmet descriptors, variable height may be null with `not_required`; requesting diagnostics never requires an otherwise unnecessary predominant height. Never substitute zeros as measured descriptors. |
| `buildings`, `barriers`, `reflectors` | Resolved object IDs, solid/face geometry, face IDs, outward or designated normals, reflection sides and material coefficients. |
| `misc_regions` | Region IDs, resolved method/profile and normalized geometry/parameters. |

Prepared source/zone geometry uses the same raw `position`, `vertices` or
`triangles` forms defined for inputs. Inferred points include their original
inference IDs; no result ID depends on scheduler order. Explicit/generated
receiver records in extended profiles include ground elevation and physical
height in addition to acoustic XYZ. These data allow round-trip persistence
without treating acoustic Z as ground, recreating a different roof or guessing
terrain under a long-term source centre.

The `diagnostics` object must state `ground_method`,
`octave_ground_approximation` (boolean), `assessment_quantity`,
`integration_converged` (boolean), `omission_basis`, and `unbounded_prefix_cuts`.
`diagnostics.integration_summary` aggregates every extended-source zone
integration of the run:
- `reports`: the requested `integration_reports`;
- counts: `zone_integrations`, `not_converged`, `accepted_cells`,
  `initial_partition_cells` and `obstructed_topology_cells`;
- maxima: `maximum_refinement_depth` and `maximum_boundary_cells`;
- per-band energy totals: `coarse_fine_discrepancy_energy`,
  `boundary_interval_energy` and `obstructed_topology_energy`;
- `largest_relative_discrepancy`: the integration with the largest
  (discrepancy + boundary interval) / zone energy in any band. It gives the
  ratio, the band, the source, zone and receiver IDs, the reflection faces, and
  whether that integration converged.

It also carries the same error statement as the individual reports. The summary
is independent of how the work was divided or where it ran.

`diagnostics.integration` always lists the source-receiver and receiver-total
tolerance reports described below. Per-zone reports appear there only with
`output.integration_reports: "full"`.
An extended-source integration report includes original source/zone IDs, accepted
cell counts and maximum refinement depth, the requested tolerances, and whether
the error statement is a coarse/fine discrepancy or a certified interval.
Do not relabel an ordinary convergence test as an integral error guarantee.
At a depth, float64-subdivision or live-cell limit, unresolved coverage keeps
its trial energy; its conservative bound must fit the tolerance of the complete
source-receiver energy (all families), otherwise the job fails. Such zones report
`integration_converged: false` with `boundary_tolerance_scope:
"source_receiver_total"`, and the source-receiver check adds a report with the
deferred allowance and the energies it was compared with. Reports also give
`obstructed_topology_cells`/`obstructed_topology_energy`, and per-band
`boundary_interval_energy` and `boundary_coverage_measure`.

A point-source family whose joint contact search is incomplete keeps its found
routes; the route alternatives it may have missed are bounded by the family's
unscreened cap minus the capped found energy. That bound is included in the
source's and the receiver's `omitted_energy`, and the receiver must satisfy
bound ≤ absolute + relative × (computed + cut-off energy) in every band or the
job fails. Accepted receivers get an `integration` report with
`boundary_tolerance_scope: "receiver_total"`, `unresolved_contact_allowance`,
`receiver_energy`, `receiver_omitted_energy`, the tolerances and
`first_unresolved_contact` (the first diagnostic message).

### Pathway warnings

Feature `pathway_warnings`: a problem confined to one source/receiver/
reflection-family transfer does not fail the job. Codes `calculation_failed`,
`nonfinite_result` and `contact_search_incomplete` raised there are pathway
errors; every other error still fails the job. A receiver with pathway problems
has `warnings`, an array of objects with `kind`, `bounded` and `message`
(diagnostic text with engine IDs), plus `source_id`, `receiver_id`,
`reflection_faces` or energies where known:

| `kind` | `bounded` | Meaning |
| --- | --- | --- |
| `failed_pathway` | false | The family's energy is excluded; the receiver has `complete: false`, and an explicit receiver's source contribution has `complete: false`. Levels may be understated. |
| `unresolved_pathways_above_tolerance` | true | The missed-route bound (already in `omitted_energy`) exceeds the receiver-total tolerance; includes `allowance`. |
| `integration_tolerance_exceeded` | true | An extended-source boundary allowance exceeds the source-receiver tolerance; trial energy kept; includes `allowance`. |

`complete` is omitted when true. The result has `warning_summary` with counts:
`warnings`, `receivers_with_warnings`, `incomplete_receivers`,
`failed_pathways` and `bounded_above_tolerance`. The QGIS frontend completes
the run, lists the counts and up to three named examples with the run, and
retains every warning in the receiver results.

### Omission coverage and path ledgers

When advertising `omission_bounds`, return `omission` on each receiver and source
contribution, with eight-band arrays:

| Field | Meaning |
| --- | --- |
| `numerical_energy` | Nonnegative energy of evaluated contributions actually discarded; same quantity as legacy `omitted_energy`. |
| `upper_energy` | Certified total upper bound covering **all** omitted work, per band; null if any omitted coverage lacks such a bound. Zero only for proved zero/no omissions. |
| `coverage_state` | Per band: `none`, `bounded`, or `unbounded`. Numerically evaluated omissions without a certified remaining integral bound are `unbounded`. |
| `increase_bound_db` | For a finite upper bound U and E > 0, `10 log10(1 + U/E)`; zero for E = U = 0; null if U is unavailable or E = 0 < U. |
| `upper_level_db` | `10 log10(E + U)` when U is finite, omissions exist and E + U > 0; otherwise null. |

`omission_basis` identifies whether the legacy fields contain computed numerical
energies only; they cannot represent unknown work as a guarantee of zero.
Contributions eliminated solely through positive bounds or unbounded heuristics
still have `cutoff_limited` band state when nothing is retained. Unknown or
uncalculated energy is never `exact_zero`. Cap reductions and physically
ineligible paths are not omissions. Neither report bounds missing physics,
unsupported inputs or excluded reflection orders.

Summary/full path diagnostics identify receiver, original source, zone if
applicable, ordered reflection face sequence and band. Each omission coverage
record additionally identifies the disjoint source-domain/cell or subtree,
omission test, numerical energy if evaluated, upper bound if certified and any
unbounded reason. Parent coverage replaced by children is not counted again;
source portions sum consistently to receiver accounting.

Full ledgers retain final physical reflection/diffraction contacts, the separate
screening-suppressed ground/reference trace, route kind/eligibility, term
distances and octave `L_W`, `D_c`, `D_omega`, `A_div`, `A_atm`, `A_gr`, `A_bar`,
`A_refl`, `A_misc`, and `C_met`. Record route energy, unscreened reference energy,
pre/post-cap family energy, integration weights and retained/omitted disposition.
An exact-zero term has an explicit reason; do not serialize an infinite dB loss
as JSON infinity or silently turn a numerical failure into null. Interim pruning
bounces are identified separately and never shown as completed physical paths.

## Transfer reuse and inverse calculations

The optional `source_receiver_transfers` feature supports reusable homogeneous
source-to-point-receiver contributions through the ordinary asynchronous
`POST /v1/calculations` lifecycle. It does not perform an inverse fit. Clients
must discover this feature before requesting transfers; forward support alone
never implies transfer or measurement-fitting support.

A transfer request preserves the full scene, source identity, geometry,
attachments, directivity and power basis, and supplies:

- `output.source_power_transfers: true`;
- exactly `100` dB in all eight bands of each source's `spectrum`, interpreted in
  that source's normal `power_basis` (`total_power`, `power_per_m`, or
  `power_per_m2`), without pre-applying length or area corrections;
- explicit `settings.minimum_contribution_db: null`, absent/null
  `settings.pruning_strategy`, and `settings.area_absolute_tolerance: 0`;
- the general ground method (explicit `general` or its default), homogeneous
  sources without `zones`, and explicit point receivers without receiver areas.

These guards are validated before submission. Relative-only integration avoids
an absolute-energy floor that changes integration accuracy when fitted powers
change. Homogeneous source centres and long-term meteorological descriptors stay
fixed as octave power changes. Zoned sources are rejected rather than assuming
that heterogeneous power-weighted centres remain fixed. The mean-path-height
method is excluded because it supports A-weighted assessment rather than octave
fitting. Other ordinary scene and numerical restrictions still apply.

Completed transfer results use the ordinary `receivers[].sources[]` contribution
shape and add this top-level descriptor:

```json
"source_power_transfers": {
  "reference_db": 100.0,
  "power_basis": "per_source_input",
  "pruning": "disabled"
}
```

For each calculated octave, subtract `reference_db` from its source contribution
`octave_db` to obtain a transfer in dB, including the declared source power basis.
An `exact_zero` band represents a genuinely zero transfer. A client must validate
identities, descriptor, settings and band states, and reject transfers with
warnings, incomplete pathways or any omitted energy before fitting. Disabling
pruning does not turn uncertain or failed pathway evaluation into exact silence.
The original request and engine/build identity define the transfer dependencies;
changes to geometry, terrain, atmosphere, material, directivity, source basis or
numerical settings require reevaluation.

The client retains measurement IDs/locations, observed octave levels and unknown
status, source-to-spectrum assignments, locked/adjustable status and initial
values, objective and convergence diagnostics. Shared spectrum assignments remain
shared parameters. Unknown observations and adjustable current values are never
submitted as forward power or zero energy. The QGIS client fits complete octave
observations by nonnegative least squares with locked background contributions,
shows proposals, and applies them only after checking the authored snapshot.
No engine-side inverse solver, uncertainty estimate, independent field-agreement
validation or additional ISO conformity claim is supplied by this feature.

## Diagnostics and status codes

Synchronous errors use:

```json
{
  "error": {
    "code": "unsupported",
    "message": "This source/path combination is outside the implemented profile.",
    "field": "sources[0]"
  }
}
```

`code` and `message` are required strings; `field` is an optional human-readable
input location. A failed/cancelled snapshot stores this same object in its
`error` member. Clients key behavior on state/status/code, not English wording.

Status codes are acalc-engine's (its API.md has the full table). engine-iso's
rejections before acceptance map to `400 invalid_json` and `422 invalid_input` /
`unsupported`:

| HTTP | Codes / meaning |
| --- | --- |
| `400` | `invalid_json`: malformed JSON, incorrect structural types, unknown fields, trailing objects. |
| `401` | `unauthorized`: a missing or wrong API key (acalc-engine). |
| `403` | `browser_origin_rejected`: browser-origin access is unavailable. |
| `404` | `not_found`: unknown, evicted or pre-restart calculation ID. |
| `409` | `results_unavailable`: calculation has no completed result. |
| `413` | `request_too_large`: body exceeds the configured limit. |
| `415` | `content_type`: JSON media type required. |
| `422` | `invalid_input` or `unsupported`: invalid acoustic values or unsupported capability detected before acceptance. |
| `503` | `job_capacity` with `Retry-After`, `shutting_down`, or `engine_unavailable`. |
| `500` | Internal transport/serialization failure. |

Failures after acceptance appear in an HTTP-200 status snapshot with
`state: "failed"`, not an HTTP-500 status-poll response. Diagnostic codes include
`calculation_failed`, `unsupported`, `invalid_input`, `nonfinite_result`,
`internal_error`, or acalc-engine's `engine_stopped` when the engine program
stopped. Cancellation uses `cancelled`. Numerical work/depth limits
and unresolved propagation are failures, never successful truncated results.

## Retention, shutdown and client workflow

acalc-engine's defaults: maximum JSON body 32 MiB; 16 running and 128 retained
jobs (set on its admin pages); in-memory storage
only. When capacity is needed, evict the oldest terminal job. Never evict an
active job to accept another. If every slot is active, return 503. Fetch and
persist completed results promptly. Restart loses all jobs; an unknown ID does
not prove that its former calculation failed. Graceful shutdown cancels active
jobs and closes streams.

The QGIS frontend allows up to 1 GiB and five minutes for a completed-result
GET, including retries of that same GET. Other requests keep their 128 MiB
response limit and 15-second timeout. Result size grows with source–receiver
contributions and can greatly exceed the submitted model size (a
2,800-source / 369-receiver result is about 668 MiB). These are frontend
transport limits, not acoustic or service limits. Oversized or interrupted
downloads do not replace saved results or resubmit the calculation. JSON
decoding and persistence still require memory proportional to the result size.

A frontend should:

1. Read capabilities, check required inputs/combinations, and preserve the
   chosen input snapshot and method/settings for reproducibility.
2. POST once and retain the returned ID and links.
3. Subscribe to events or poll status; use task-point percent for the progress
   bar and phase text only as activity information.
4. On `completed`, fetch and persist results. On `failed`, show the diagnostic;
   on `cancelled`, stop without creating result layers.
5. On user cancellation, DELETE the returned status URL. On network interruption,
   recover the existing job through GET instead of starting a duplicate.

Both backends may use their native schedulers while preserving this wire
contract, deterministic acoustic reduction, cancellation and task-point semantics.
Worker-pool design and thread/goroutine counts are not API inputs.
