//! Area-receiver grids as engine-held GeoTIFF artifacts.
//!
//! The solver reports every generated grid sample as a receiver. For HTTP
//! transport those samples are moved into one Float32 GeoTIFF per area (dBA,
//! then the 63–8000 Hz octave bands), so a large grid is downloaded separately
//! from the run's result document. Pixels are centred on the calculated
//! samples; excluded candidates and zero-energy levels are NoData, never
//! interpolated. Coordinates are the request's Cartesian metres, without a CRS.

use crate::types::Diagnostic;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const NODATA: f32 = f32::MIN;
pub const BAND_NAMES: [&str; 9] = [
    "dBA", "63 Hz", "125 Hz", "250 Hz", "500 Hz", "1000 Hz", "2000 Hz", "4000 Hz", "8000 Hz",
];
pub const MEDIA_TYPE: &str = "image/tiff";
const WARNING_EXAMPLES: usize = 8;

pub struct AreaRaster {
    pub name: String,
    pub bytes: Vec<u8>,
}

struct Grid {
    columns: usize,
    rows: usize,
    pixels: Vec<f32>,
    filled: Vec<bool>,
    samples: usize,
    with_warnings: usize,
    incomplete: usize,
    bounded: usize,
    failed: usize,
    kinds: Map<String, Value>,
    examples: Vec<Value>,
}

fn internal(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("internal_error", message)
}

/// Move area samples out of `result["receivers"]` into GeoTIFF artifacts.
///
/// Each `area_receivers` summary gains a `raster` descriptor (including the
/// `href` produced for its artifact name) and a `sample_warnings` digest that
/// preserves the run's pathway-warning evidence without per-sample records.
pub fn split_area_rasters(
    result: &mut Value,
    href: impl Fn(&str) -> String,
) -> Result<Vec<AreaRaster>, Diagnostic> {
    let summaries = result["area_receivers"]
        .as_array()
        .ok_or_else(|| internal("Result has no area summaries"))?;
    let mut index = HashMap::new();
    let mut grids = Vec::with_capacity(summaries.len());
    for (i, summary) in summaries.iter().enumerate() {
        let id = summary["area_receiver_id"]
            .as_str()
            .ok_or_else(|| internal("Area summary has no identity"))?;
        index.insert(id.to_string(), i);
        let (columns, rows) = (
            summary["grid"]["columns"].as_u64().unwrap_or(0) as usize,
            summary["grid"]["rows"].as_u64().unwrap_or(0) as usize,
        );
        if columns == 0 || rows == 0 {
            return Err(internal("Area summary has no grid dimensions"));
        }
        grids.push(Grid {
            columns,
            rows,
            pixels: vec![NODATA; 9 * columns * rows],
            filled: vec![false; columns * rows],
            samples: 0,
            with_warnings: 0,
            incomplete: 0,
            bounded: 0,
            failed: 0,
            kinds: Map::new(),
            examples: vec![],
        });
    }
    let receivers = match result["receivers"].take() {
        Value::Array(receivers) => receivers,
        _ => return Err(internal("Result has no receiver array")),
    };
    let mut points = Vec::with_capacity(receivers.len());
    for receiver in receivers {
        let Some(area) = receiver.get("area_receiver_id").and_then(Value::as_str) else {
            points.push(receiver);
            continue;
        };
        let grid = index
            .get(area)
            .map(|&i| &mut grids[i])
            .ok_or_else(|| internal("Area sample has no area summary"))?;
        let (column, row) = (
            receiver["grid_column"].as_u64().unwrap_or(u64::MAX) as usize,
            receiver["grid_row"].as_u64().unwrap_or(u64::MAX) as usize,
        );
        if column >= grid.columns || row >= grid.rows {
            return Err(internal("Area sample lies outside its grid"));
        }
        // Raster rows run north to south; grid rows count from the south edge.
        let cell = (grid.rows - 1 - row) * grid.columns + column;
        if std::mem::replace(&mut grid.filled[cell], true) {
            return Err(internal("Two area samples share one grid cell"));
        }
        grid.samples += 1;
        let octaves = receiver["octave_db"].as_array();
        for band in 0..9 {
            let level = if band == 0 {
                &receiver["a_weighted_db"]
            } else {
                octaves
                    .and_then(|o| o.get(band - 1))
                    .unwrap_or(&Value::Null)
            };
            if let Some(level) = level.as_f64() {
                grid.pixels[band * grid.columns * grid.rows + cell] = level as f32;
            }
        }
        if receiver.get("complete") == Some(&json!(false)) {
            grid.incomplete += 1;
        }
        if let Some(warnings) = receiver.get("warnings").and_then(Value::as_array) {
            grid.with_warnings += 1;
            if warnings.iter().any(|w| w["bounded"] == true) {
                grid.bounded += 1;
            }
            for warning in warnings {
                let kind = warning["kind"].as_str().unwrap_or("unknown").to_string();
                if kind == "failed_pathway" {
                    grid.failed += 1;
                }
                let count = grid.kinds.get(&kind).and_then(Value::as_u64).unwrap_or(0);
                grid.kinds.insert(kind, json!(count + 1));
                if grid.examples.len() < WARNING_EXAMPLES
                    && !grid
                        .examples
                        .iter()
                        .any(|e| e["message"] == warning["message"])
                {
                    grid.examples.push(warning.clone());
                }
            }
        }
    }
    result["receivers"] = Value::Array(points);
    let mut rasters = Vec::with_capacity(grids.len());
    let summaries = result["area_receivers"].as_array_mut().unwrap();
    for (i, (summary, grid)) in summaries.iter_mut().zip(grids).enumerate() {
        if summary["receiver_count"].as_u64() != Some(grid.samples as u64) {
            return Err(internal("Area sample count disagrees with its summary"));
        }
        let spacing = summary["spacing_m"].as_f64().unwrap_or(0.);
        let origin = &summary["grid"]["origin"];
        let (x, y) = (origin[0].as_f64(), origin[1].as_f64());
        let (Some(x), Some(y)) = (x, y) else {
            return Err(internal("Area summary has no grid origin"));
        };
        let transform = [
            x - spacing / 2.,
            spacing,
            0.,
            y + (grid.rows as f64 - 0.5) * spacing,
            0.,
            -spacing,
        ];
        let bytes = geotiff(grid.columns, grid.rows, &transform, &grid.pixels);
        let name = format!("area-{i}.tif");
        summary["raster"] = json!({
            "artifact": name, "href": href(&name), "media_type": MEDIA_TYPE, "format": "GeoTIFF",
            "bytes": bytes.len(), "sha256": format!("{:x}", Sha256::digest(&bytes)),
            "columns": grid.columns, "rows": grid.rows, "data_type": "Float32",
            "bands": BAND_NAMES, "band_units": ["dBA","dB","dB","dB","dB","dB","dB","dB","dB"],
            "nodata": NODATA as f64, "geotransform": transform, "pixel_is_area": true,
            "coordinates": "request Cartesian metres; no CRS",
            "nodata_meaning": "No calculated sample (outside boundary, excluded, inside a building) or zero calculated energy in that band",
        });
        summary["sample_warnings"] = json!({
            "samples_with_warnings": grid.with_warnings, "incomplete_samples": grid.incomplete,
            "bounded_samples": grid.bounded, "failed_pathways": grid.failed,
            "warning_counts": grid.kinds, "examples": grid.examples,
        });
        rasters.push(AreaRaster { name, bytes });
    }
    Ok(rasters)
}

/// Minimal little-endian classic GeoTIFF: separate Float32 planes in strips.
fn geotiff(columns: usize, rows: usize, transform: &[f64; 6], pixels: &[f32]) -> Vec<u8> {
    const SHORT: u16 = 3;
    const LONG: u16 = 4;
    const ASCII: u16 = 2;
    const DOUBLE: u16 = 12;
    let bands = BAND_NAMES.len();
    let row_bytes = columns * 4;
    let rows_per_strip = (65536 / row_bytes).clamp(1, rows);
    let strips = rows.div_ceil(rows_per_strip);
    let data_start = 8usize;
    let mut out = Vec::with_capacity(data_start + pixels.len() * 4 + 4096);
    out.extend_from_slice(b"II*\0\0\0\0\0");
    for value in pixels {
        out.extend_from_slice(&value.to_le_bytes());
    }
    let (mut offsets, mut counts) = (vec![], vec![]);
    for band in 0..bands {
        for strip in 0..strips {
            let first = strip * rows_per_strip;
            let height = rows_per_strip.min(rows - first);
            offsets.push((data_start + (band * rows + first) * row_bytes) as u32);
            counts.push((height * row_bytes) as u32);
        }
    }
    let shorts = |v: &[u16]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>();
    let longs = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>();
    let doubles = |v: &[f64]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>();
    let ascii = |s: &str| {
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        b
    };
    let mut metadata = String::from("<GDALMetadata>");
    for (band, name) in BAND_NAMES.iter().enumerate() {
        let unit = if band == 0 { "dBA" } else { "dB" };
        metadata += &format!(
            "<Item name=\"DESCRIPTION\" sample=\"{band}\" role=\"description\">{name}</Item>\
             <Item name=\"UNITTYPE\" sample=\"{band}\" role=\"unittype\">{unit}</Item>"
        );
    }
    metadata += "</GDALMetadata>";
    // Tags must be in ascending order. GeoKeys declare only PixelIsArea: the
    // engine has no CRS, which the client assigns from its own model.
    let entries: Vec<(u16, u16, u32, Vec<u8>)> = vec![
        (256, LONG, 1, longs(&[columns as u32])),
        (257, LONG, 1, longs(&[rows as u32])),
        (258, SHORT, bands as u32, shorts(&vec![32; bands])),
        (259, SHORT, 1, shorts(&[1])),
        (262, SHORT, 1, shorts(&[1])),
        (273, LONG, offsets.len() as u32, longs(&offsets)),
        (277, SHORT, 1, shorts(&[bands as u16])),
        (278, LONG, 1, longs(&[rows_per_strip as u32])),
        (279, LONG, counts.len() as u32, longs(&counts)),
        (284, SHORT, 1, shorts(&[2])),
        (338, SHORT, bands as u32 - 1, shorts(&vec![0; bands - 1])),
        (339, SHORT, bands as u32, shorts(&vec![3; bands])),
        (
            33550,
            DOUBLE,
            3,
            doubles(&[transform[1], -transform[5], 0.]),
        ),
        (
            33922,
            DOUBLE,
            6,
            doubles(&[0., 0., 0., transform[0], transform[3], 0.]),
        ),
        (34735, SHORT, 8, shorts(&[1, 1, 0, 1, 1025, 0, 1, 1])),
        (42112, ASCII, metadata.len() as u32 + 1, ascii(&metadata)),
        (42113, ASCII, 0, ascii(&format!("{:e}", NODATA as f64))),
    ];
    // Out-of-line values follow the pixels; the directory comes last.
    let mut inline = Vec::with_capacity(entries.len());
    for (tag, kind, count, value) in &entries {
        let count = if *kind == ASCII {
            value.len() as u32
        } else {
            *count
        };
        if value.len() <= 4 {
            let mut field = [0u8; 4];
            field[..value.len()].copy_from_slice(value);
            inline.push((*tag, *kind, count, field));
        } else {
            if out.len() % 2 == 1 {
                out.push(0);
            }
            let offset = (out.len() as u32).to_le_bytes();
            out.extend_from_slice(value);
            inline.push((*tag, *kind, count, offset));
        }
    }
    if out.len() % 2 == 1 {
        out.push(0);
    }
    let directory = out.len() as u32;
    out[4..8].copy_from_slice(&directory.to_le_bytes());
    out.extend_from_slice(&(inline.len() as u16).to_le_bytes());
    for (tag, kind, count, field) in inline {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&field);
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(column: u64, row: u64, level: Option<f64>) -> Value {
        json!({"receiver_id":format!("area:{column}:{row}"),"area_receiver_id":"grid",
               "grid_column":column,"grid_row":row,"a_weighted_db":level,
               "octave_db":[level,null,null,null,null,null,null,level.map(|l| l - 1.)]})
    }

    #[test]
    fn samples_become_north_up_planes_and_points_remain() {
        let mut result = json!({
            "receivers":[{"receiver_id":"p","a_weighted_db":40.}, sample(0,0,Some(50.)), sample(2,1,Some(60.)),
                         {"receiver_id":"area:1:0","area_receiver_id":"grid","grid_column":1,"grid_row":0,
                          "a_weighted_db":null,"octave_db":[null,null,null,null,null,null,null,null],
                          "complete":false,"warnings":[{"kind":"failed_pathway","bounded":false,"message":"m"}]}],
            "area_receivers":[{"area_receiver_id":"grid","receiver_count":3,"spacing_m":2.,"height_m":1.5,
                               "grid":{"origin":[10.,20.],"columns":3,"rows":2}}]
        });
        let rasters = split_area_rasters(&mut result, |n| format!("/x/{n}")).unwrap();
        assert_eq!(result["receivers"].as_array().unwrap().len(), 1);
        let summary = &result["area_receivers"][0];
        assert_eq!(summary["raster"]["href"], "/x/area-0.tif");
        assert_eq!(
            summary["raster"]["geotransform"],
            json!([9., 2., 0., 23., 0., -2.])
        );
        assert_eq!(summary["sample_warnings"]["incomplete_samples"], 1);
        assert_eq!(summary["sample_warnings"]["failed_pathways"], 1);
        assert_eq!(
            summary["sample_warnings"]["warning_counts"]["failed_pathway"],
            1
        );
        let bytes = &rasters[0].bytes;
        assert_eq!(summary["raster"]["bytes"], bytes.len());
        assert_eq!(&bytes[..4], b"II*\0");
        let pixel = |band: usize, row: usize, column: usize| {
            let at = 8 + 4 * (band * 6 + row * 3 + column);
            f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
        };
        // Grid row 0 is the southern raster row.
        assert_eq!(pixel(0, 1, 0), 50.);
        assert_eq!(pixel(0, 0, 2), 60.);
        assert_eq!(pixel(8, 0, 2), 59.);
        assert_eq!(pixel(1, 0, 2), 60.);
        assert_eq!(pixel(2, 0, 2), NODATA);
        assert_eq!(pixel(0, 1, 1), NODATA);
        assert_eq!(pixel(0, 0, 0), NODATA);
    }

    #[test]
    fn inconsistent_samples_are_internal_errors() {
        let base = json!({"receivers":[sample(0,0,Some(1.)), sample(0,0,Some(2.))],
            "area_receivers":[{"area_receiver_id":"grid","receiver_count":2,"spacing_m":1.,
                               "grid":{"origin":[0.,0.],"columns":1,"rows":1}}]});
        assert!(split_area_rasters(&mut base.clone(), |n| n.into()).is_err());
        let mut outside = base.clone();
        outside["receivers"] = json!([sample(1, 0, Some(1.))]);
        outside["area_receivers"][0]["receiver_count"] = json!(1);
        assert!(split_area_rasters(&mut outside, |n| n.into()).is_err());
        let mut miscounted = base;
        miscounted["receivers"] = json!([sample(0, 0, Some(1.))]);
        assert!(split_area_rasters(&mut miscounted, |n| n.into()).is_err());
    }
}
