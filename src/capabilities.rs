use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
pub fn algorithm_revision() -> String {
    format!(
        "sha256:{:x}",
        Sha256::digest(include_bytes!("../docs/ALGORITHM.md"))
    )
}
pub fn build_revision() -> &'static str {
    static REV: OnceLock<String> = OnceLock::new();
    REV.get_or_init(|| {
        let mut hash = Sha256::new();
        for file in [
            include_str!("types.rs"),
            include_str!("spatial.rs"),
            include_str!("kernels.rs"),
            include_str!("acoustics.rs"),
            include_str!("geometry.rs"),
            include_str!("scene.rs"),
            include_str!("foliage.rs"),
            include_str!("path.rs"),
            include_str!("joint_contact.rs"),
            include_str!("beam.rs"),
            include_str!("lateral.rs"),
            include_str!("integration.rs"),
            include_str!("solver.rs"),
            include_str!("validation.rs"),
            include_str!("request.rs"),
            include_str!("engine.rs"),
            include_str!("area_raster.rs"),
            include_str!("capabilities.rs"),
            include_str!("cli.rs"),
            include_str!("main.rs"),
            include_str!("../Cargo.toml"),
            include_str!("../Cargo.lock"),
            include_str!("../docs/API.md"),
        ] {
            hash.update(file.as_bytes());
            hash.update([0]);
        }
        if let Some(kernels) = crate::kernels::installed() {
            hash.update(kernels.revision().as_bytes());
        }
        format!("sha256:{:x}", hash.finalize())
    })
}
fn components() -> Value {
    let xyz = json!({"type":"array","minItems":3,"maxItems":3,"items":{"type":"number"}});
    let xy = json!({"type":"array","minItems":2,"maxItems":2,"items":{"type":"number"}});
    let ground_ref = json!({"oneOf":[{"type":"object","required":["position"],"additionalProperties":false,"properties":{"position":xyz}},{"type":"object","required":["inference_id"],"additionalProperties":false,"properties":{"inference_id":{"type":"string","minLength":1}}}]});
    let polygon = json!({"type":"array","minItems":3,"items":xy});
    json!([
    {"id":"acalc_schema11","revision":"iso-rust-cdt-spade-2.15.1-lexicographic-v1","category":"terrain_construction","input_schema":{"type":"object","additionalProperties":false,"properties":{"construction_profile":{"const":"acalc_schema11"},"samples":{"type":"array","items":xyz},"breaklines":{"type":"array","items":{"type":"array","minItems":2,"items":xyz}},"inference_points":{"type":"array","items":{"type":"object","required":["id","position_xy"],"additionalProperties":false,"properties":{"id":{"type":"string","minLength":1},"position_xy":xy}}},"breakline_definitions":{"type":"array","items":{"type":"object","required":["id","vertices"],"additionalProperties":false,"properties":{"id":{"type":"string","minLength":1},"vertices":{"type":"array","minItems":2,"items":ground_ref}}}}}},"restrictions":["2.5D ground; immutable initial surface for all inference targets; explicit surveyed elevations cannot conflict by more than 1e-7m","Sorted insertion and pinned Spade version; cross-backend terrain equivalence remains unverified","Nearest-hull extension; singleton and collinear coverage conventions as API.md"],"verification_status":"partially_verified"},
    {"id":"acalc_schema11","revision":"iso-rust-face-placement-v3","category":"attachment_placement","input_schema":{"type":"object","required":["placement_profile","x_m","y_m"],"additionalProperties":false,"properties":{"placement_profile":{"const":"acalc_schema11"},"frame":{"enum":["building_local","roof_top_edge","legacy_world"]},"x_m":{"type":"number","minimum":0},"y_m":{"type":"number","minimum":0},"mounting":{"enum":["surface","standoff"]},"distance_m":{"type":"number","minimum":0},"width_m":{"type":"number","exclusiveMinimum":0},"height_m":{"type":"number","exclusiveMinimum":0}},"oneOf":[{"required":["mounting"],"properties":{"mounting":{"const":"surface"},"distance_m":{"const":0}},"not":{"anyOf":[{"required":["width_m"]},{"required":["height_m"]}]}},{"required":["mounting","distance_m"],"properties":{"mounting":{"const":"standoff"},"distance_m":{"minimum":0.05}},"not":{"anyOf":[{"required":["width_m"]},{"required":["height_m"]}]}},{"required":["width_m","height_m"],"not":{"anyOf":[{"required":["distance_m"]},{"required":["mounting"]}]}}]},"restrictions":["Authored face identities retained; planar whole-rectangle containment required; surface points and rectangles use the outward physical launch restriction and no host reflection; standoff points are ordinary points at least 0.05 m from the face, reflected by the host; any point source 1e-7 to 0.05 m from a building face it projects onto is rejected"],"verification_status":"partially_verified"},
    {"id":"iso9613_2024_foliage_simplified_acalc_v1","revision":"iso-rust-foliage-arc5000-v1","category":"miscellaneous_attenuation","input_schema":{"type":"object","required":["id","kind","method_profile","boundary","canopy_height_m"],"additionalProperties":false,"properties":{"id":{"type":"string","minLength":1},"kind":{"const":"foliage"},"method_profile":{"const":"iso9613_2024_foliage_simplified_acalc_v1"},"boundary":polygon,"canopy_height_m":{"type":"number","exclusiveMinimum":0}}},"restrictions":["Dense vegetation applicability is an input declaration","5km arc; no relevant vertical ray, chord >=10km or XY-turnback arc; clear vertical-wall bounces preserve one unfolded arc","Overlapping volumes united; 10–20m short row, >20m per-metre row capped at200m; below10m no table loss"],"verification_status":"partially_verified"}
    ])
}
pub fn component_profiles_for(request: &Value) -> Value {
    let terrain = request
        .get("terrain")
        .and_then(|terrain| terrain.get("construction_profile"))
        .and_then(Value::as_str)
        .is_some_and(|profile| profile == "acalc_schema11");
    let attachment = request
        .get("sources")
        .and_then(Value::as_array)
        .is_some_and(|sources| {
            sources
                .iter()
                .any(|source| source.get("attachment").is_some())
        });
    let foliage = request
        .get("misc_regions")
        .and_then(Value::as_array)
        .is_some_and(|regions| !regions.is_empty());
    components()
        .as_array()
        .into_iter()
        .flatten()
        .filter(|profile| match profile["category"].as_str() {
            Some("terrain_construction") => terrain,
            Some("attachment_placement") => attachment,
            Some("miscellaneous_attenuation") => foliage,
            _ => false,
        })
        .cloned()
        .collect::<Vec<_>>()
        .into()
}
pub fn capabilities() -> Value {
    let features = json!([
        "line_sources",
        "emission_zones",
        "terrain_samples",
        "terrain_inference",
        "mixed_ground",
        "building_solids",
        "building_roof_mesh",
        "building_attached_sources",
        "polyline_barriers",
        "reflecting_barriers",
        "planar_reflectors",
        "directivity_tables",
        "directivity_orientation_matrix",
        "roof_reflections",
        "nonvertical_reflections",
        "foliage_simple",
        "mean_path_height",
        "extended_long_term",
        "reflected_long_term",
        "irregular_terrain_long_term",
        "reflected_screening",
        "screened_extended_sources",
        "reflected_extended_sources",
        "area_receiver_totals",
        "area_receiver_rasters",
        "pathway_warnings",
        "prepared_geometry",
        "path_diagnostics",
        "integration_reports",
        "pruning_strategy",
        "source_receiver_transfers"
    ]);
    let limitations = json!([
        "No declaration of ISO 17534 conformity. Published complete ISO/TR 17534-3 scene cases have not been verified for this 2024 implementation.",
        "Ground method is user-selected; irregular terrain is retained and does not prevent general-ground or long-term calculation. Their applicability on irregular terrain is an unverified engineering extension. Mean_path_height requires its porous-ground, non-pure-tone A-weighted applicability declarations.",
        "Screened nonvertical reflections and nonvertical reflected long-term correction are unsupported.",
        "Reflected vertical screening, lateral/mixed section treatment, bent mean-height ground and the total-contribution cap follow ALGORITHM.md engine interpretations; their full independent scene verification is pending.",
        "Joint finite-edge contact solving uses automatic support discovery with full route validation. Exhaustive topology coverage and global minimum-path discovery are unverified. An incomplete search keeps its found routes and reports the family-cap bound of what it may have missed as omitted energy.",
        "Building-surface input power describes exterior radiation; no automatic +3 dB host gain or zero-length host reflection is added.",
        "The special long-term industrial-screening recommendation of ISO 9613-2:2024 7.4.1 Note1 is excluded; ordinary ground/screening coupling remains.",
        "Industrial/housing aggregate regions, detailed forest models, raw directivity-catalog conversion and inverse fitting have no implemented profile. The source_receiver_transfers capability supplies reference contributions for client-side fitting of homogeneous source powers; it does not perform an inverse fit.",
        "Source-first finite-portal beams query a 3D receiver index. Sources and reflection families are evaluated independently in parallel. No reflector-prefix or queue heuristic energy pruning is used.",
        "Legacy receiver_margin_db is accepted for compatibility but ignored; receiver-relative pruning has been removed.",
        "Pruning strategies dba/dbc are automatic and empirical: before each reflection order, contributions whose weighted upper bound is below the quietest receiver's weighted level less 10 log10(source count) + 15 dB (dBA) or + 20 dB (dBC) are skipped. Weighted totals were within about 0.01 dB of unpruned results in the recorded tests; individual octave bands can lose more. The absolute cutoff prunes only before path construction, on conservative upper bounds: the unscreened family cap for point families with vertical reflectors, and summed cell bounds for line/area families. Computed contributions are never discarded. Point families with non-vertical reflectors are not pruned. Reported omissions are those bounds.",
        "Reflection order and numerical integration settings require convergence review for the application. A problem confined to one source/receiver/reflection pathway does not stop the run: it is reported as a receiver warning, bounded (in omitted energy) or unbounded (receiver marked incomplete). Other errors stop the calculation.",
        "Extended sources use positive adaptive quadrature with conservative transition treatment; reports distinguish discrepancy checks from certified unresolved-boundary coverage.",
        "Terrain triangulation and attachment revision identities are Rust-specific; identical preparation across backends remains unverified."
    ]);
    json!({"api_version":"1.2","algorithm_revision":algorithm_revision(),"implementation":{"name":"iso_rust","version":crate::engine::version(),"build_revision":build_revision()},"method":"ISO 9613-2:2024","atmospheric_absorption":"ISO 9613-1:1993","quality_guidance":["ISO 17534-1:2015 quality framework; accessible clauses reviewed, full declaration provisions unverified","ISO/TR 17534-3:2015 recommendations reviewed against 2024; 1996 reference cases are not inherited passes","docs/TERRAIN.md records terrain preparation conventions"],"verified_conformity":false,"features":features,"profiles":[{"id":"iso_rust_2024","features":features,"ground_methods":["general","mean_path_height"],"meteorological_modes":["downwind","long_term"],"restrictions":limitations,"verification_status":"partially_verified"}],"component_profiles":components(),"ground_methods":["general","mean_path_height"],"source_kinds":["point","line","area"],"receiver_kinds":["point","area"],"coordinates":"Cartesian metres, one vertical datum; acoustic XYZ distinct from ground references","ground":"Deterministic staged 2.5D constrained terrain; ground regions support ordered_last_wins and nonoverlapping_schema11","ground_region_policies":["ordered_last_wins","nonoverlapping_schema11"],"frequencies_hz":[63,125,250,500,1000,2000,4000,8000],"max_reflect_order":5,"pruning_strategies":["dba","dbc"],"progress_task_points":{"preparation":1,"point_source":1,"line_source":2,"area_source":3},"limitations":limitations,"profile_limits":{"max_sources":10000,"max_triangles_per_source":10000,"max_receiver_areas":10000,"max_polygon_vertices":10000,"max_grid_candidates":1000000,"max_faces":10000,"max_path_diagnostic_records":100000,"max_area_depth":24,"coordinate_magnitude_m":1000000000,"sound_power_db_min":-200,"sound_power_db_max":300,"directivity_orthonormality_tolerance":1e-8,"max_terrain_observations":250000,"max_joint_contact_supports":16,"max_joint_contact_sequences_per_choice":256,"max_joint_contact_boxes_per_sequence":20000,"joint_contact_enclosure_width_m":0.01}})
}
