#![allow(clippy::possible_missing_else, clippy::needless_range_loop)]
#![allow(clippy::too_many_arguments)]
//! Positive, power-conserving integration of one original-source/family domain.
//! Geometric classification is independent of quadrature-node agreement.
use crate::{
    geometry::*,
    path::{self, Family, Work},
    scene::*,
    types::*,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub struct Integrated {
    pub energy: Bands,
    pub ledgers: Vec<Value>,
    pub reports: Vec<ZoneReport>,
    /// Zones that reached the depth or live-cell limit before their unresolved
    /// boundary coverage fitted the family's own tolerance. The allowance is the
    /// accepted-cell discrepancy plus the conservative interval bound of that
    /// coverage; the caller must fit it within the source-receiver tolerance.
    pub unresolved: Option<Unresolved>,
}
pub struct Unresolved {
    pub allowance: Bands,
    pub message: String,
}
/// One zone's integration toward one receiver through one reflection family.
/// `output.integration_reports: "full"` lists each as JSON; by default they
/// are only aggregated in a `Summary`.
pub struct ZoneReport {
    /// Index in the source's zones.
    pub zone: usize,
    pub accepted_cells: usize,
    pub initial_partition_cells: usize,
    pub maximum_refinement_depth: usize,
    pub boundary_cells: usize,
    pub obstructed_topology_cells: usize,
    pub absolute_tolerance: f64,
    pub energy: Bands,
    pub discrepancy: Bands,
    pub boundary_interval: Bands,
    pub boundary_coverage: Bands,
    pub obstructed_energy: Bands,
    pub converged: bool,
}
const ERROR_STATEMENT: &str = "coarse/fine discrepancy for classified cells; conservative integration interval for unresolved boundary coverage";
impl ZoneReport {
    pub fn json(
        &self,
        scene: &Scene,
        source: &Source,
        receiver: &Receiver,
        family: &Family,
    ) -> Value {
        json!({"source_id":source.id,"zone_id":source.zones[self.zone].id,"receiver_id":receiver.id,
            "reflection_faces":family.iter().map(|i|scene.faces[*i].key()).collect::<Vec<_>>(),
            "accepted_cells":self.accepted_cells,"initial_partition_cells":self.initial_partition_cells,
            "maximum_refinement_depth":self.maximum_refinement_depth,"absolute_tolerance":self.absolute_tolerance,
            "relative_tolerance":scene.settings.relative,"coarse_fine_discrepancy_energy":self.discrepancy,
            "boundary_interval_energy":self.boundary_interval,"boundary_coverage_measure":self.boundary_coverage,
            "boundary_cells":self.boundary_cells,"obstructed_topology_cells":self.obstructed_topology_cells,
            "obstructed_topology_energy":self.obstructed_energy,"error_statement":ERROR_STATEMENT,
            "certified_total_integral_error":false,"integration_converged":self.converged,
            "boundary_tolerance_scope":if self.converged {"family"} else {"source_receiver_total"}})
    }
}

/// The zone integrations of a calculation or part of one, reduced to totals,
/// maxima and the worst case, so the result's size does not grow with the
/// number of integrations. Sums are exact and the worst case is chosen by a
/// total order, so a summary does not depend on how the work was divided.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Summary {
    integrations: usize,
    not_converged: usize,
    accepted_cells: usize,
    initial_partition_cells: usize,
    obstructed_topology_cells: usize,
    maximum_refinement_depth: usize,
    maximum_boundary_cells: usize,
    discrepancy: [Exact; 8],
    boundary_interval: [Exact; 8],
    obstructed_energy: [Exact; 8],
    largest: Option<Largest>,
}
/// The integration with the largest (discrepancy + boundary interval) /
/// energy in any band with energy.
#[derive(Clone, Serialize, Deserialize)]
struct Largest {
    #[serde(with = "f64_bits")]
    ratio: f64,
    band: usize,
    source: String,
    receiver: usize,
    family: Family,
    zone: usize,
    converged: bool,
}
impl Largest {
    /// Larger ratio first; ties by source id, receiver, family, zone.
    fn beats(&self, other: &Largest) -> bool {
        other
            .ratio
            .total_cmp(&self.ratio)
            .then_with(|| self.source.cmp(&other.source))
            .then_with(|| self.receiver.cmp(&other.receiver))
            .then_with(|| crate::beam::family_order(&self.family, &other.family))
            .then_with(|| self.zone.cmp(&other.zone))
            .then_with(|| self.band.cmp(&other.band))
            .is_lt()
    }
}
impl Summary {
    pub fn add(&mut self, source: &Source, receiver: usize, family: &Family, r: &ZoneReport) {
        self.integrations += 1;
        self.not_converged += usize::from(!r.converged);
        self.accepted_cells += r.accepted_cells;
        self.initial_partition_cells += r.initial_partition_cells;
        self.obstructed_topology_cells += r.obstructed_topology_cells;
        self.maximum_refinement_depth = self
            .maximum_refinement_depth
            .max(r.maximum_refinement_depth);
        self.maximum_boundary_cells = self.maximum_boundary_cells.max(r.boundary_cells);
        for b in 0..8 {
            self.discrepancy[b].add(r.discrepancy[b]);
            self.boundary_interval[b].add(r.boundary_interval[b]);
            self.obstructed_energy[b].add(r.obstructed_energy[b]);
        }
        let worst = (0..8)
            .filter(|b| r.energy[*b] > 0.)
            .map(|b| ((r.discrepancy[b] + r.boundary_interval[b]) / r.energy[b], b))
            .max_by(|a, b| a.0.total_cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
        if let Some((ratio, band)) = worst {
            self.offer(Largest {
                ratio,
                band,
                source: source.id.clone(),
                receiver,
                family: family.clone(),
                zone: r.zone,
                converged: r.converged,
            });
        }
    }
    fn offer(&mut self, candidate: Largest) {
        if self.largest.as_ref().is_none_or(|l| candidate.beats(l)) {
            self.largest = Some(candidate);
        }
    }
    pub fn merge(&mut self, other: Summary) {
        self.integrations += other.integrations;
        self.not_converged += other.not_converged;
        self.accepted_cells += other.accepted_cells;
        self.initial_partition_cells += other.initial_partition_cells;
        self.obstructed_topology_cells += other.obstructed_topology_cells;
        self.maximum_refinement_depth = self
            .maximum_refinement_depth
            .max(other.maximum_refinement_depth);
        self.maximum_boundary_cells = self
            .maximum_boundary_cells
            .max(other.maximum_boundary_cells);
        for b in 0..8 {
            self.discrepancy[b].merge(&other.discrepancy[b]);
            self.boundary_interval[b].merge(&other.boundary_interval[b]);
            self.obstructed_energy[b].merge(&other.obstructed_energy[b]);
        }
        if let Some(candidate) = other.largest {
            self.offer(candidate);
        }
    }
    pub fn is_empty(&self) -> bool {
        self.integrations == 0
    }
    pub fn json(&self, scene: &Scene, reports: &str) -> Value {
        let largest = self.largest.as_ref().map(|l| {
            let source = scene.sources.iter().find(|s| s.id == l.source);
            let hz = [63, 125, 250, 500, 1000, 2000, 4000, 8000][l.band];
            json!({"ratio":l.ratio,"octave_band_hz":hz,
                "source_id":l.source,"zone_id":source.map(|s| s.zones[l.zone].id.clone()),
                "receiver_id":scene.receivers[l.receiver].id,
                "reflection_faces":l.family.iter().map(|i|scene.faces[*i].key()).collect::<Vec<_>>(),
                "integration_converged":l.converged})
        });
        json!({"reports":reports,"zone_integrations":self.integrations,"not_converged":self.not_converged,
            "accepted_cells":self.accepted_cells,"initial_partition_cells":self.initial_partition_cells,
            "obstructed_topology_cells":self.obstructed_topology_cells,
            "maximum_refinement_depth":self.maximum_refinement_depth,
            "maximum_boundary_cells":self.maximum_boundary_cells,
            "coarse_fine_discrepancy_energy":self.discrepancy.each_ref().map(Exact::total),
            "boundary_interval_energy":self.boundary_interval.each_ref().map(Exact::total),
            "obstructed_topology_energy":self.obstructed_energy.each_ref().map(Exact::total),
            "largest_relative_discrepancy":largest,
            "relative_discrepancy_basis":"(coarse/fine discrepancy + boundary interval) / integrated zone energy, largest band",
            "error_statement":ERROR_STATEMENT,"certified_total_integral_error":false})
    }
}
const MAX_PARTITION_CELLS: usize = 10_000;
/// Adaptive cells held at once for one zone/family/receiver. A band jump along
/// a curve needs about U0/tolerance boundary cells before its interval fits.
const MAX_LIVE_CELLS: usize = 50_000;
const GEOM_EPS: f64 = 1e-8;

#[derive(Clone, Copy)]
struct Plane {
    normal: Point,
    offset: f64,
}
impl Plane {
    fn through(a: Point, b: Point, c: Point) -> Option<Self> {
        let n = cross(sub(b, a), sub(c, a));
        let length = norm(n);
        if length < 1e-12 {
            return None;
        }
        let mut n = scale(n, 1. / length);
        let first = n.iter().find(|x| x.abs() > 1e-15)?;
        if *first < 0. {
            n = scale(n, -1.);
        }
        Some(Self {
            normal: n,
            offset: dot(n, a),
        })
    }
    fn signed(self, p: Point) -> f64 {
        dot(self.normal, p) - self.offset
    }
}
#[derive(Clone, Copy)]
struct Node {
    cell: Cell,
    depth: usize,
    /// Bands still being integrated on this cell. Bands are accepted or closed
    /// independently, so a jump confined to some bands (reflector eligibility)
    /// does not keep refining the others.
    active: [bool; 8],
    /// Weighted transfer at the centroid, already sampled by the parent.
    coarse: Option<Bands>,
}
#[derive(Clone, Copy, PartialEq)]
enum Classification {
    Impossible,
    Smooth,
    /// No transition plane, contact boundary or reflector-size jump lies in the
    /// cell, but obstacles, foliage or terrain along the swept paths keep the
    /// screening/foliage topology from being proven constant.
    Obstructed,
    Boundary,
}
struct Estimate {
    node: Node,
    energy: Bands,
    error: Bands,
    upper: Bands,
    ledgers: Vec<Value>,
    /// Bands whose geometry is not classified on this cell.
    boundary: [bool; 8],
    obstructed: bool,
    extent_ok: bool,
    zero: bool,
    /// The cell cannot be subdivided at float64 precision; it stays in the
    /// unresolved coverage with its trial energy and conservative bound.
    frozen: bool,
    /// Weighted child-centroid transfers, in `Cell::split` order; they are the
    /// children's coarse values when the cell is refined.
    samples: Vec<Bands>,
}

/// Conservative upper bound on the whole family's received energy: the sum of
/// every zone cell's `upper_bound` at the unfolded receiver. It needs no path
/// construction, so the absolute cutoff can skip integration before any work.
pub fn family_bound(
    scene: &Scene,
    source: &Source,
    receiver: &Receiver,
    family: &Family,
) -> Result<Bands, Diagnostic> {
    let image = unfold(scene, receiver.point, family);
    let mut sums = [Sum::default(); 8];
    for zone in &source.zones {
        for cell in &zone.cells {
            let bound = upper_bound(scene, zone, family, *cell, image)?;
            for b in 0..8 {
                sums[b].add(bound[b]);
            }
        }
    }
    Ok(sums.map(Sum::total))
}

/// `pruned` bands are not integrated; they receive no energy.
pub fn integrate(
    scene: &Scene,
    source: &Source,
    receiver: &Receiver,
    family: &Family,
    pruned: [bool; 8],
    work: &Work,
) -> Result<Integrated, Diagnostic> {
    work.tick()?;
    let mut sums = [Sum::default(); 8];
    let mut ledgers = Vec::new();
    let mut reports = Vec::new();
    let mut unresolved: Option<Unresolved> = None;
    let source_measure: f64 = source.zones.iter().map(|z| z.measure).sum();
    for (zi, zone) in source.zones.iter().enumerate() {
        work.tick()?;
        // Silencing pruned bands skips their route construction and keeps
        // them out of refinement; acceptance tolerances keep their scale.
        let masked;
        let zone = if pruned.iter().any(|p| *p) {
            let mut z = zone.clone();
            for b in 0..8 {
                if pruned[b] {
                    z.power[b] = 0.;
                }
            }
            if z.power.iter().all(|p| *p == 0.) {
                continue;
            }
            masked = z;
            &masked
        } else {
            zone
        };
        if source.kind == "point" {
            let p = zone.cells[0].centroid();
            let transfer = path::transfer(scene, source, zone, p, receiver, family, work)?;
            for (b, sum) in sums.iter_mut().enumerate() {
                sum.add(product(
                    zone.power[b],
                    transfer.energy[b],
                    "Point source contribution",
                )?);
            }
            ledgers.extend(transfer.ledgers);
            continue;
        }
        let result = integrate_zone(
            scene,
            source,
            zone,
            receiver,
            family,
            work,
            scene.settings.absolute * zone.measure / source_measure,
        )?;
        for (b, sum) in sums.iter_mut().enumerate() {
            sum.add(result.energy[b]);
        }
        ledgers.extend(result.ledgers);
        reports.extend(
            result
                .reports
                .into_iter()
                .map(|r| ZoneReport { zone: zi, ..r }),
        );
        if let Some(zone_unresolved) = result.unresolved {
            match &mut unresolved {
                Some(u) => {
                    for b in 0..8 {
                        u.allowance[b] += zone_unresolved.allowance[b];
                    }
                }
                None => unresolved = Some(zone_unresolved),
            }
        }
    }
    let energy = sums.map(Sum::total);
    if energy.iter().any(|e| !e.is_finite()) {
        return Err(Diagnostic::numerical(
            "Integrated family accumulation overflow",
        ));
    }
    Ok(Integrated {
        energy,
        ledgers,
        reports,
        unresolved,
    })
}

fn integrate_zone(
    scene: &Scene,
    source: &Source,
    zone: &Zone,
    receiver: &Receiver,
    family: &Family,
    work: &Work,
    absolute: f64,
) -> Result<Integrated, Diagnostic> {
    let virtual_receiver = unfold(scene, receiver.point, family);
    let planes = transition_planes(scene, virtual_receiver, family, work)?;
    let mut cells = zone.cells.clone();
    for plane in &planes {
        work.tick()?;
        let mut next = Vec::new();
        for cell in cells {
            next.extend(split_plane(cell, *plane)?);
            if next.len() > MAX_PARTITION_CELLS {
                return Err(Diagnostic::new(
                    "calculation_failed",
                    "Geometric integration partition exceeds live-cell limit",
                ));
            }
        }
        cells = next;
    }
    let partition_measure: f64 = cells.iter().map(Cell::measure).sum();
    if (partition_measure - zone.measure).abs() > 1e-10 * zone.measure.max(1.) {
        return Err(Diagnostic::numerical(
            "Geometric partition did not preserve source measure",
        ));
    }
    let initial_cells = cells.len();
    let mut frontier: Vec<_> = cells
        .into_iter()
        .map(|cell| Node {
            cell,
            depth: 0,
            active: [true; 8],
            coarse: None,
        })
        .collect();
    let mut energy = [Sum::default(); 8];
    let mut discrepancy = [Sum::default(); 8];
    let mut ledgers = Vec::new();
    let mut accepted = 0usize;
    let mut depth = 0usize;
    let mut boundary_measure = [0.; 8];
    let mut boundary_bound = [0.; 8];
    let mut boundary_cells = 0usize;
    let mut obstructed_cells = 0usize;
    let mut obstructed_energy = [Sum::default(); 8];
    let mut deferred: Option<Unresolved> = None;
    // Unresolved cells with their trial values: not yet accepted in some band.
    let mut held: Vec<Estimate> = Vec::new();
    // Bands whose unresolved coverage has been closed by the interval test.
    let mut closed = [false; 8];
    while !frontier.is_empty() {
        work.tick()?;
        // Parallel cells own all trial values and ledgers. Indexed collection
        // preserves input order; floating-point reduction remains deterministic.
        let evaluated: Result<Vec<Estimate>, Diagnostic> = frontier
            .par_iter()
            .map(|node| {
                work.isolated(|work| {
                    estimate(
                        scene,
                        source,
                        zone,
                        receiver,
                        family,
                        *node,
                        virtual_receiver,
                        &planes,
                        absolute,
                        work,
                    )
                })
            })
            .collect();
        for mut e in evaluated? {
            work.tick()?;
            depth = depth.max(e.node.depth);
            let mut taken = [false; 8];
            for b in 0..8 {
                if !e.node.active[b] {
                    continue;
                }
                if e.zero
                    || (!e.boundary[b]
                        && e.extent_ok
                        && e.error[b]
                            <= 0.5
                                * (absolute * e.node.cell.measure() / zone.measure
                                    + scene.settings.relative * e.energy[b]))
                {
                    energy[b].add(e.energy[b]);
                    discrepancy[b].add(e.error[b]);
                    if e.obstructed {
                        obstructed_energy[b].add(e.energy[b]);
                    }
                    taken[b] = true;
                    e.node.active[b] = false;
                }
            }
            ledgers.extend(take_band_ledgers(&mut e.ledgers, &taken));
            if e.node.active.iter().any(|a| *a) {
                held.push(e);
            } else {
                obstructed_cells += usize::from(e.obstructed);
                ledgers.extend(e.ledgers);
                accepted += 1;
            }
        }
        if held.is_empty() {
            break;
        }
        let mut upper = [Sum::default(); 8];
        let mut trial = [Sum::default(); 8];
        for cell in &held {
            for b in 0..8 {
                if cell.node.active[b] {
                    upper[b].add(cell.upper[b]);
                    trial[b].add(cell.energy[b]);
                }
            }
        }
        let upper = upper.map(Sum::total);
        // This tests the complete unresolved coverage of each band, not one
        // quiet sample. The true integral and permitted positive quadrature
        // both lie in [0,U], so U also bounds their absolute discrepancy.
        let extent_ok = held.iter().all(|e| e.extent_ok);
        let tolerance: [f64; 8] = std::array::from_fn(|b| {
            absolute + scene.settings.relative * (energy[b].total() + trial[b].total())
        });
        let fits: [bool; 8] = std::array::from_fn(|b| {
            !closed[b]
                && extent_ok
                && upper[b].is_finite()
                && discrepancy[b].total() + upper[b] <= tolerance[b]
        });
        if fits.iter().any(|f| *f) {
            for b in 0..8 {
                if fits[b] {
                    boundary_bound[b] = upper[b];
                    closed[b] = true;
                }
            }
            boundary_cells = boundary_cells.max(held.len());
            let mut still_held = Vec::new();
            for mut e in held {
                let mut taken = [false; 8];
                for b in 0..8 {
                    if fits[b] && e.node.active[b] {
                        boundary_measure[b] += e.node.cell.measure();
                        energy[b].add(e.energy[b]);
                        taken[b] = true;
                        e.node.active[b] = false;
                    }
                }
                ledgers.extend(take_band_ledgers(&mut e.ledgers, &taken));
                if e.node.active.iter().any(|a| *a) {
                    still_held.push(e);
                } else {
                    ledgers.extend(e.ledgers);
                    accepted += 1;
                }
            }
            held = still_held;
            if held.is_empty() {
                break;
            }
        }
        // Split every refinable cell while the live-cell budget allows; at the
        // budget, split the cells with the largest share of the open bands'
        // tolerance first (stable order on ties), and hold the rest.
        let refinable = |e: &Estimate| !e.frozen && e.node.depth < scene.settings.max_depth;
        let children_of = |e: &Estimate| match e.node.cell {
            Cell::Line(_) => 2,
            _ => 4,
        };
        let mut order: Vec<usize> = (0..held.len()).filter(|i| refinable(&held[*i])).collect();
        let growth: usize = order.iter().map(|i| children_of(&held[*i]) - 1).sum();
        if held.len() + growth > MAX_LIVE_CELLS {
            let share = |e: &Estimate| {
                (0..8)
                    .filter(|b| e.node.active[*b])
                    .map(|b| e.upper[b] / tolerance[b].max(f64::MIN_POSITIVE))
                    .fold(0., f64::max)
            };
            order.sort_by(|a, b| share(&held[*b]).total_cmp(&share(&held[*a])));
            let mut live = held.len();
            order.retain(|i| {
                let extra = children_of(&held[*i]) - 1;
                if live + extra <= MAX_LIVE_CELLS {
                    live += extra;
                    true
                } else {
                    false
                }
            });
            order.sort_unstable();
        }
        if order.is_empty() {
            let depth_limited = held
                .iter()
                .any(|e| e.node.depth >= scene.settings.max_depth);
            let live_limited = held.iter().any(refinable);
            if !extent_ok {
                return Err(Diagnostic::new(
                    "calculation_failed",
                    if live_limited {
                        "Adaptive integration exceeds live-cell limit; coverage was not truncated"
                            .to_string()
                    } else {
                        format!(
                            "Source {} zone {}: integration depth limit with unresolved geometry or energy discrepancy; no converged result is available",
                            source.id, zone.id
                        )
                    },
                ));
            }
            // Coverage stays complete: every held cell contributes its trial
            // energy, and [0,U] bounds both it and the true integral. Whether
            // U fits is decided against the source-receiver total.
            let mut allowance = [0.; 8];
            for b in 0..8 {
                if !closed[b] {
                    allowance[b] = discrepancy[b].total() + upper[b];
                    boundary_bound[b] = upper[b];
                }
            }
            boundary_cells = boundary_cells.max(held.len());
            for e in held {
                for b in 0..8 {
                    if e.node.active[b] {
                        boundary_measure[b] += e.node.cell.measure();
                        energy[b].add(e.energy[b]);
                    }
                }
                ledgers.extend(e.ledgers);
                accepted += 1;
            }
            deferred = Some(Unresolved {
                allowance,
                message: format!(
                    "Source {} zone {}: {} with unresolved boundary coverage; its conservative interval does not fit the source-receiver tolerance",
                    source.id,
                    zone.id,
                    if live_limited {
                        "adaptive live-cell limit"
                    } else if depth_limited {
                        "integration depth limit"
                    } else {
                        "float64 subdivision limit"
                    }
                ),
            });
            break;
        }
        let mut next = Vec::new();
        let mut kept = Vec::new();
        let mut selected = order.into_iter().peekable();
        for (i, e) in held.into_iter().enumerate() {
            if selected.peek() == Some(&i) {
                selected.next();
                for (k, child) in e.node.cell.split()?.into_iter().enumerate() {
                    next.push(Node {
                        cell: child,
                        depth: e.node.depth + 1,
                        active: e.node.active,
                        coarse: e.samples.get(k).copied(),
                    });
                }
            } else {
                kept.push(e);
            }
        }
        held = kept;
        frontier = next;
    }
    let energy = energy.map(Sum::total);
    let report = ZoneReport {
        zone: 0,
        accepted_cells: accepted,
        initial_partition_cells: initial_cells,
        maximum_refinement_depth: depth,
        boundary_cells,
        obstructed_topology_cells: obstructed_cells,
        absolute_tolerance: absolute,
        energy,
        discrepancy: discrepancy.map(Sum::total),
        boundary_interval: boundary_bound,
        boundary_coverage: boundary_measure,
        obstructed_energy: obstructed_energy.map(Sum::total),
        converged: deferred.is_none(),
    };
    Ok(Integrated {
        energy,
        ledgers,
        reports: vec![report],
        unresolved: deferred,
    })
}

#[allow(clippy::too_many_arguments)]
fn estimate(
    scene: &Scene,
    source: &Source,
    zone: &Zone,
    receiver: &Receiver,
    family: &Family,
    node: Node,
    virtual_receiver: Point,
    planes: &[Plane],
    absolute: f64,
    work: &Work,
) -> Result<Estimate, Diagnostic> {
    work.tick()?;
    let (classification, size_unresolved) = classify(
        scene,
        zone,
        receiver,
        family,
        node.cell,
        virtual_receiver,
        planes,
        work,
    )?;
    if classification == Classification::Impossible {
        return Ok(Estimate {
            node,
            energy: [0.; 8],
            error: [0.; 8],
            upper: [0.; 8],
            ledgers: vec![],
            boundary: [false; 8],
            obstructed: false,
            extent_ok: true,
            zero: true,
            frozen: false,
            samples: vec![],
        });
    }
    let distance = minimum_distance(node.cell, virtual_receiver);
    let extent_ok = node.cell.extent() <= scene.settings.factor * distance
        && scene
            .settings
            .max_extent
            .is_none_or(|maximum| node.cell.extent() <= maximum);
    let upper = upper_bound(scene, zone, family, node.cell, virtual_receiver)?;
    let centroid = || {
        path::transfer(
            scene,
            source,
            zone,
            node.cell.centroid(),
            receiver,
            family,
            work,
        )
    };
    let coarse = match node.coarse {
        Some(coarse) => coarse,
        None => weight_transfer(centroid()?.energy, zone, node.cell.measure())?,
    };
    let Ok(children) = node.cell.split() else {
        let coarse_ledgers = centroid()?.ledgers;
        return Ok(Estimate {
            node,
            energy: coarse,
            error: [0.; 8],
            upper,
            ledgers: coarse_ledgers,
            boundary: [true; 8],
            obstructed: false,
            extent_ok,
            zero: false,
            frozen: true,
            samples: vec![],
        });
    };
    let samples: Result<Vec<_>, Diagnostic> = children
        .par_iter()
        .map(|child| {
            let transfer = work.isolated(|work| {
                path::transfer(
                    scene,
                    source,
                    zone,
                    child.centroid(),
                    receiver,
                    family,
                    work,
                )
            })?;
            Ok::<_, Diagnostic>((
                weight_transfer(transfer.energy, zone, child.measure())?,
                transfer.ledgers,
            ))
        })
        .collect();
    let mut sums = [Sum::default(); 8];
    let mut ledgers = Vec::new();
    let mut child_energy = Vec::with_capacity(children.len());
    for (e, l) in samples? {
        work.tick()?;
        for b in 0..8 {
            sums[b].add(e[b]);
        }
        ledgers.extend(l);
        child_energy.push(e);
    }
    let energy = sums.map(Sum::total);
    // The bound is proportional to emitted measure. Child measures of a
    // sliver can exceed the parent's by rounding; the samples are weighted by
    // the children, so the bound must cover that weight as well.
    let weight: f64 = children.iter().map(Cell::measure).sum();
    let upper = upper.map(|u| u * (weight / node.cell.measure()).max(1.));
    let mut error = [0.; 8];
    for b in 0..8 {
        error[b] = (energy[b] - coarse[b]).abs();
        if !energy[b].is_finite() {
            return Err(Diagnostic::numerical("Integration quadrature overflow"));
        }
        if upper[b].is_finite() && energy[b] > upper[b] * (1. + 1e-10) + absolute * 1e-12 {
            return Err(Diagnostic::numerical(
                "Quadrature violates its conservative contribution bound",
            ));
        }
    }
    Ok(Estimate {
        node,
        energy,
        error,
        upper,
        ledgers,
        boundary: std::array::from_fn(|b| {
            classification == Classification::Boundary || size_unresolved[b]
        }),
        obstructed: classification == Classification::Obstructed,
        extent_ok,
        zero: false,
        frozen: false,
        samples: child_energy,
    })
}

/// Removes and returns the band ledgers of `bands`; band-free entries stay
/// until the cell is complete.
fn take_band_ledgers(ledgers: &mut Vec<Value>, bands: &[bool; 8]) -> Vec<Value> {
    if !bands.iter().any(|b| *b) {
        return Vec::new();
    }
    let (taken, kept) = std::mem::take(ledgers).into_iter().partition(|l| {
        l["band"]
            .as_u64()
            .is_some_and(|b| bands.get(b as usize).copied().unwrap_or(false))
    });
    *ledgers = kept;
    taken
}

fn product(a: f64, b: f64, label: &str) -> Result<f64, Diagnostic> {
    if a == 0. || b == 0. {
        Ok(0.)
    } else {
        positive(a * b, label)
    }
}
fn weight_transfer(transfer: Bands, zone: &Zone, measure: f64) -> Result<Bands, Diagnostic> {
    let fraction = positive(measure / zone.measure, "Cell measure share")?;
    let mut out = [0.; 8];
    for b in 0..8 {
        out[b] = product(
            product(zone.power[b], fraction, "Cell emitted power")?,
            transfer[b],
            "Weighted quadrature transfer",
        )?;
    }
    Ok(out)
}

fn upper_bound(
    scene: &Scene,
    zone: &Zone,
    family: &Family,
    cell: Cell,
    receiver_image: Point,
) -> Result<Bands, Diagnostic> {
    let coordinate_scale = cell
        .vertices()
        .iter()
        .flatten()
        .chain(receiver_image.iter())
        .map(|x| x.abs())
        .fold(1., f64::max);
    let distance =
        (minimum_distance(cell, receiver_image) - 128. * f64::EPSILON * coordinate_scale).max(0.);
    let directivity = zone
        .directivity
        .as_ref()
        .map(Directivity::maximum_correction)
        .unwrap_or([0.; 8]);
    let mut out = [0.; 8];
    for b in 0..8 {
        if zone.power[b] == 0. {
            continue;
        }
        let mut material = 1.;
        for &i in family {
            material *= 1. - scene.faces[i].absorption[b];
        }
        if material == 0. {
            continue;
        }
        if distance == 0. {
            out[b] = f64::INFINITY;
            continue;
        }
        let emitted = product(
            zone.power[b],
            cell.measure() / zone.measure,
            "Bound cell power",
        )?;
        // The family's total-contribution cap bounds every screened completion.
        // -6 dB covers general net ground; simplified net ground is >=-3.011.
        // Meteorology and miscellaneous losses are nonnegative. Atmospheric
        // absorption uses the unfolded reference distance, which is at least
        // the cell's minimum distance to the receiver image.
        let logarithm = emitted.log10() + material.log10() + (directivity[b] + 6. - 11.) / 10.
            - 2. * distance.log10()
            - scene.alpha[b] * distance / 10.;
        // Underflow means the bound is below the smallest normal number,
        // which then remains a valid (positive) upper bound.
        out[b] = (10f64.powf(logarithm) * (1. + 1e-12)).max(f64::MIN_POSITIVE);
    }
    Ok(out)
}

fn minimum_distance(cell: Cell, p: Point) -> f64 {
    match cell {
        Cell::Point(a) => distance(a, p),
        Cell::Line([a, b]) => point_segment_distance(p, a, b),
        Cell::Triangle(t) => {
            let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
            let l = norm(n);
            if l == 0. {
                return 0.;
            }
            let n = scale(n, 1. / l);
            let signed = dot(sub(p, t[0]), n);
            let q = sub(p, scale(n, signed));
            if point_on_triangle(q, t) {
                signed.abs()
            } else {
                cell_edges(&cell)
                    .iter()
                    .map(|[a, b]| point_segment_distance(p, *a, *b))
                    .fold(f64::INFINITY, f64::min)
            }
        }
    }
}
fn unfold(scene: &Scene, mut p: Point, prefix: &[usize]) -> Point {
    for &i in prefix.iter().rev() {
        p = scene.faces[i].mirror(p);
    }
    p
}

fn transition_planes(
    scene: &Scene,
    receiver: Point,
    family: &Family,
    work: &Work,
) -> Result<Vec<Plane>, Diagnostic> {
    let mut result = Vec::new();
    for leg in 0..=family.len() {
        let prefix = &family[..leg];
        for f in &scene.faces {
            work.tick()?;
            let vertices: Vec<_> = f
                .vertices
                .iter()
                .map(|p| unfold(scene, *p, prefix))
                .collect();
            for i in 0..vertices.len() {
                if let Some(p) =
                    Plane::through(receiver, vertices[i], vertices[(i + 1) % vertices.len()])
                {
                    result.push(p);
                }
            }
            if let Some(p) = Plane::through(vertices[0], vertices[1], vertices[2]) {
                result.push(p);
            }
        }
        for polygon in scene
            .regions
            .iter()
            .map(|r| &r.polygon)
            .chain(scene.foliage.iter().map(|r| &r.polygon))
        {
            for i in 0..polygon.len() {
                work.tick()?;
                let a = unfold(scene, polygon[i], prefix);
                let b = unfold(scene, polygon[(i + 1) % polygon.len()], prefix);
                let up = unfold(scene, add(polygon[i], [0., 0., 1.]), prefix);
                if let Some(p) = Plane::through(receiver, a, up) {
                    result.push(p);
                }
                if let Some(p) = Plane::through(a, b, up) {
                    result.push(p);
                }
            }
        }
        // Ridge/ground-profile topology changes when a ray crosses a mesh edge
        // or vertex. These planes partition geometry, not emitted power.
        if !scene.terrain.is_planar() {
            for t in &scene.terrain.triangles {
                for i in 0..3 {
                    work.tick()?;
                    let a = unfold(scene, scene.terrain.vertices[t[i]], prefix);
                    let b = unfold(scene, scene.terrain.vertices[t[(i + 1) % 3]], prefix);
                    if let Some(p) = Plane::through(receiver, a, b) {
                        result.push(p);
                    }
                }
            }
        }
    }
    result.sort_by(|a, b| {
        a.normal[0]
            .total_cmp(&b.normal[0])
            .then(a.normal[1].total_cmp(&b.normal[1]))
            .then(a.normal[2].total_cmp(&b.normal[2]))
            .then(a.offset.total_cmp(&b.offset))
    });
    result.dedup_by(|a, b| a.normal == b.normal && a.offset == b.offset);
    Ok(result)
}

fn split_plane(cell: Cell, plane: Plane) -> Result<Vec<Cell>, Diagnostic> {
    let points = cell.vertices();
    let signed: Vec<_> = points.iter().map(|p| plane.signed(*p)).collect();
    if !signed.iter().any(|d| *d > GEOM_EPS) || !signed.iter().any(|d| *d < -GEOM_EPS) {
        return Ok(vec![cell]);
    }
    match cell {
        Cell::Point(_) => Ok(vec![cell]),
        Cell::Line([a, b]) => {
            let t = signed[0] / (signed[0] - signed[1]);
            let mid = lerp(a, b, t);
            if distance(a, mid) == 0. || distance(b, mid) == 0. {
                return Err(Diagnostic::numerical(
                    "Geometric line partition exhausted coordinate precision",
                ));
            }
            Ok(vec![Cell::Line([a, mid]), Cell::Line([mid, b])])
        }
        Cell::Triangle(t) => {
            let mut out = Vec::new();
            for sign in [1., -1.] {
                let mut polygon = Vec::new();
                for i in 0..3 {
                    let a = t[i];
                    let b = t[(i + 1) % 3];
                    let da = plane.signed(a) * sign;
                    let db = plane.signed(b) * sign;
                    if da >= 0. {
                        polygon.push(a);
                    }
                    if (da < 0.) != (db < 0.) {
                        polygon.push(lerp(a, b, da / (da - db)));
                    }
                }
                polygon.dedup();
                if polygon.len() > 1 && polygon.first() == polygon.last() {
                    polygon.pop();
                }
                for i in 1..polygon.len().saturating_sub(1) {
                    let child = Cell::Triangle([polygon[0], polygon[i], polygon[i + 1]]);
                    if child.measure() > 0. {
                        out.push(child);
                    }
                }
            }
            let measure: f64 = out.iter().map(Cell::measure).sum();
            if (measure - cell.measure()).abs() > 1e-10 * cell.measure().max(1.) {
                return Err(Diagnostic::numerical(
                    "Geometric area partition lost source measure",
                ));
            }
            // A sliver whose part on one side has no representable area stays
            // whole; it remains a boundary cell with its own bound.
            Ok(if out.len() < 2 { vec![cell] } else { out })
        }
    }
}

fn classify(
    scene: &Scene,
    zone: &Zone,
    receiver: &Receiver,
    family: &Family,
    cell: Cell,
    virtual_receiver: Point,
    planes: &[Plane],
    work: &Work,
) -> Result<(Classification, [bool; 8]), Diagnostic> {
    let vertices = cell.vertices();
    if zone.power.iter().all(|w| *w == 0.)
        || family
            .iter()
            .any(|i| scene.faces[*i].absorption.iter().all(|a| *a == 1.))
    {
        return Ok((Classification::Impossible, [false; 8]));
    }
    let mut unresolved = false;
    let mut obstructed = false;
    let mut size_unresolved = [false; 8];
    // In unfolded space all mandatory contacts lie on one straight ray. Each
    // contact parameter is constant / affine(source); with fixed denominator
    // signs, endpoint/order/side inequalities attain their signs at vertices.
    let mut previous = vec![1.; vertices.len()];
    for (k, &fi) in family.iter().enumerate() {
        let face = &scene.faces[fi];
        let origin = unfold(scene, face.origin, &family[..k]);
        let n = sub(
            unfold(scene, add(face.origin, face.normal), &family[..k]),
            origin,
        );
        let r = dot(sub(virtual_receiver, origin), n);
        let ds: Vec<_> = vertices.iter().map(|p| dot(sub(*p, origin), n)).collect();
        if (face.side == "front" && ds.iter().all(|d| *d <= 0.))
            || (face.side == "back" && ds.iter().all(|d| *d >= 0.))
        {
            return Ok((Classification::Impossible, [false; 8]));
        }
        // The contact parameter r/(r-d) lies in (0,1) only when the source and
        // receiver image are strictly on opposite sides of the face plane. The
        // signed distance is affine, so vertices decide the whole cell; this
        // also covers cells containing the parallel-ray pole d = r.
        if r != 0. && ds.iter().all(|d| d * r.signum() >= 0.) {
            return Ok((Classification::Impossible, [false; 8]));
        }
        let denominators: Vec<_> = ds.iter().map(|d| r - d).collect();
        if denominators.iter().any(|d| d.abs() < 1e-12)
            || denominators
                .iter()
                .any(|d| d.signum() != denominators[0].signum())
        {
            return Ok((Classification::Boundary, [false; 8]));
        }
        let parameter: Vec<_> = denominators.iter().map(|d| r / d).collect();
        if parameter.iter().all(|t| *t <= 0.)
            || parameter.iter().all(|t| *t >= 1.)
            || parameter.iter().zip(&previous).all(|(a, b)| a >= b)
        {
            return Ok((Classification::Impossible, [false; 8]));
        }
        if parameter.iter().any(|t| *t <= 0. || *t >= 1.)
            || parameter.iter().zip(&previous).any(|(a, b)| a >= b)
        {
            unresolved = true;
        }
        previous = parameter;
    }
    let paths: Vec<_> = vertices
        .iter()
        .map(|p| path::reference(scene, *p, receiver.point, family))
        .collect::<Result<Vec<_>, _>>()?;
    if paths.iter().any(Option::is_none) {
        if limit_contacts_miss(scene, &vertices, virtual_receiver, family) {
            return Ok((Classification::Impossible, [false; 8]));
        }
        return Ok((Classification::Boundary, [false; 8]));
    }
    let paths: Vec<Vec<Point>> = paths.into_iter().flatten().collect();
    for (k, &fi) in family.iter().enumerate() {
        let face = &scene.faces[fi];
        let contacts: Vec<_> = paths.iter().map(|p| p[k + 1]).collect();
        let local: Vec<_> = contacts
            .iter()
            .map(|p| {
                let d = sub(*p, face.origin);
                [dot(d, face.u), dot(d, face.v), 0.]
            })
            .collect();
        let convex = (0..face.projected.len()).all(|i| {
            orient(
                face.projected[i],
                face.projected[(i + 1) % face.projected.len()],
                face.projected[(i + 2) % face.projected.len()],
            ) >= 0.
        });
        if !convex {
            unresolved = true;
        } else {
            for i in 0..face.projected.len() {
                let a = face.projected[i];
                let b = face.projected[(i + 1) % face.projected.len()];
                let values: Vec<_> = local.iter().map(|p| orient(a, b, *p)).collect();
                // Contacts lie in the hull of the vertex contacts. If none is
                // inside the edge, at most an edge sliver of width GEOM_EPS
                // could reflect, and that sliver has no source measure.
                if values.iter().all(|v| *v < GEOM_EPS) && values.iter().any(|v| *v < -GEOM_EPS) {
                    return Ok((Classification::Impossible, [false; 8]));
                }
                if values.iter().any(|v| *v < -GEOM_EPS) {
                    unresolved = true;
                }
            }
        }
        let constant =
            reflector_size_constant(scene, face, &paths, k, cell, virtual_receiver, zone)?;
        for b in 0..8 {
            size_unresolved[b] |= !constant[b];
        }
    }
    for leg in 0..=family.len() {
        let sweep: Vec<_> = paths.iter().flat_map(|p| [p[leg], p[leg + 1]]).collect();
        // Terrain screening topology is partitioned by the planes through the
        // receiver image and every mesh edge; the profile below the sweep is
        // not proven constant. Planarity is not an admission gate for point
        // evaluations of the general method (ALGORITHM.md), so it is not one
        // for the samples of an extended source either.
        if !terrain_clear(scene, &sweep)? {
            obstructed = true;
        }
        // The face test only sets `obstructed`; once set, the rest cannot
        // change the classification.
        for (i, face) in scene.faces.iter().enumerate() {
            if obstructed {
                break;
            }
            work.tick()?;
            if (leg > 0 && family[leg - 1] == i) || (leg < family.len() && family[leg] == i) {
                continue;
            }
            if sweep_meets_face(face, &sweep) {
                obstructed = true;
            }
        }
        for foliage in &scene.foliage {
            if boxes_overlap(&sweep, &foliage.polygon) {
                obstructed = true;
            }
        }
    }
    if family.iter().any(|i| !scene.faces[*i].vertical())
        && (unresolved || obstructed || size_unresolved.iter().any(|u| *u))
    {
        return Err(Diagnostic::unsupported(
            "Unresolved extended-source screening with nonvertical reflection has no supported applicability profile",
        ));
    }
    if let Some(d) = &zone.directivity {
        let directions: Vec<_> = vertices.iter().map(|p| sub(virtual_receiver, *p)).collect();
        if !d.smooth_directions(&directions) {
            unresolved = true;
        }
    }
    for plane in planes {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for p in &vertices {
            let d = plane.signed(*p);
            lo = lo.min(d);
            hi = hi.max(d);
        }
        if lo < -GEOM_EPS && hi > GEOM_EPS {
            unresolved = true;
        }
    }
    let kind = if unresolved {
        Classification::Boundary
    } else if obstructed {
        Classification::Obstructed
    } else {
        Classification::Smooth
    };
    Ok((kind, size_unresolved))
}

/// A vertex whose reference contact is degenerate (it lies on a mirror plane,
/// so the contact coincides with an endpoint) has no physical route, but the
/// linear-fractional contact map is still continuous there. If every limit
/// contact of some family face lies outside one edge of that convex face, no
/// point of the cell reflects from it.
fn limit_contacts_miss(
    scene: &Scene,
    vertices: &[Point],
    virtual_receiver: Point,
    family: &Family,
) -> bool {
    for (k, &fi) in family.iter().enumerate() {
        let face = &scene.faces[fi];
        let convex = (0..face.projected.len()).all(|i| {
            orient(
                face.projected[i],
                face.projected[(i + 1) % face.projected.len()],
                face.projected[(i + 2) % face.projected.len()],
            ) >= 0.
        });
        if !convex {
            continue;
        }
        let origin = unfold(scene, face.origin, &family[..k]);
        let n = sub(
            unfold(scene, add(face.origin, face.normal), &family[..k]),
            origin,
        );
        let r = dot(sub(virtual_receiver, origin), n);
        let mut local = Vec::with_capacity(vertices.len());
        for p in vertices {
            let den = r - dot(sub(*p, origin), n);
            if den.abs() < 1e-12 {
                return false;
            }
            let t = r / den;
            if !(-1e-9..=1. + 1e-9).contains(&t) {
                return false;
            }
            let mut q = lerp(virtual_receiver, *p, t);
            for &i in &family[..k] {
                q = scene.faces[i].mirror(q);
            }
            let d = sub(q, face.origin);
            local.push([dot(d, face.u), dot(d, face.v), 0.]);
        }
        for i in 0..face.projected.len() {
            let a = face.projected[i];
            let b = face.projected[(i + 1) % face.projected.len()];
            let values: Vec<_> = local.iter().map(|p| orient(a, b, *p)).collect();
            if values.iter().all(|v| *v < GEOM_EPS) && values.iter().any(|v| *v < -GEOM_EPS) {
                return true;
            }
        }
    }
    false
}

/// Proves, per band, that equation (26) reflector eligibility is constant
/// over the cell. Bounds converge to the point values as the cell shrinks, so
/// cells away from an eligibility jump are eventually classified.
fn reflector_size_constant(
    scene: &Scene,
    face: &Face,
    paths: &[Vec<Point>],
    index: usize,
    cell: Cell,
    virtual_receiver: Point,
    zone: &Zone,
) -> Result<[bool; 8], Diagnostic> {
    let Some((lo, hi)) = effective_size_bounds(scene, face, paths, index, cell, virtual_receiver)?
    else {
        return Ok(std::array::from_fn(|b| {
            zone.power[b] == 0. || face.absorption[b] == 1.
        }));
    };
    let dmin = minimum_distance(cell, virtual_receiver);
    let dmax = cell
        .vertices()
        .iter()
        .map(|p| distance(*p, virtual_receiver))
        .fold(0., f64::max);
    let mut fraction_min: f64 = 1.;
    let mut fraction_max: f64 = 0.;
    for p in paths {
        let total: f64 = p.windows(2).map(|w| distance(w[0], w[1])).sum();
        let before: f64 = p[..=index + 1]
            .windows(2)
            .map(|w| distance(w[0], w[1]))
            .sum();
        let fraction = before / total;
        fraction_min = fraction_min.min(fraction);
        fraction_max = fraction_max.max(fraction);
    }
    // The contact parameter is linear-fractional in the source with a fixed
    // denominator sign (established by classify), so the path-length fraction
    // is monotone along every segment and its range is attained at vertices.
    let min_product = (fraction_min * (1. - fraction_min)).min(fraction_max * (1. - fraction_max));
    let max_product = if fraction_min <= 0.5 && fraction_max >= 0.5 {
        0.25
    } else {
        (fraction_min * (1. - fraction_min)).max(fraction_max * (1. - fraction_max))
    };
    let mut constant = [true; 8];
    for b in 0..8 {
        if zone.power[b] == 0. || face.absorption[b] == 1. {
            continue;
        }
        let wavelength = 340. / crate::acoustics::FREQUENCIES[b];
        let maximum_threshold = (2. * wavelength * dmax * max_product).sqrt();
        let minimum_threshold = (2. * wavelength * dmin * min_product).sqrt();
        constant[b] = lo > maximum_threshold + 1e-8 || hi <= minimum_threshold - 1e-8;
    }
    Ok(constant)
}

/// Bounds the clause 7.5.2 effective size min(a cos(beta_a), h cos(beta_h))
/// over the cell, matching the point evaluation in `path::reflection_mask`.
///
/// In unfolded space every leg is parallel to D = R' - s; the physical leg is
/// a fixed orthogonal image of D, so its components are affine in the source
/// and attain their ranges at the cell vertices, while |D| lies between the
/// exact cell distance and the largest vertex distance. Contacts are a
/// projective image of the cell with a fixed denominator sign, so they lie in
/// the hull of the vertex contacts. `None` means no bound could be formed.
fn effective_size_bounds(
    scene: &Scene,
    face: &Face,
    paths: &[Vec<Point>],
    index: usize,
    cell: Cell,
    virtual_receiver: Point,
) -> Result<Option<(f64, f64)>, Diagnostic> {
    let d_lo = minimum_distance(cell, virtual_receiver);
    let mut d_hi: f64 = 0.;
    let mut legs = Vec::with_capacity(paths.len());
    for (p, s) in paths.iter().zip(cell.vertices()) {
        let incoming = sub(p[index + 1], p[index]);
        let length = norm(incoming);
        let d = distance(s, virtual_receiver);
        if length == 0. || !length.is_finite() || d == 0. {
            return Ok(None);
        }
        d_hi = d_hi.max(d);
        legs.push(scale(incoming, d / length));
    }
    // Absolute range of an affine component over the cell.
    let range = |axis: Point| {
        let values: Vec<_> = legs.iter().map(|l| dot(*l, axis)).collect();
        let lo = values.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let magnitude = lo.abs().max(hi.abs());
        if lo <= 0. && hi >= 0. {
            (0., magnitude)
        } else {
            (lo.abs().min(hi.abs()), magnitude)
        }
    };
    // vn/hypot(vn, w) increases with vn and decreases with |w|.
    let cosine = |(n_lo, n_hi): (f64, f64), (w_lo, w_hi): (f64, f64)| {
        let lo = if n_lo == 0. {
            0.
        } else {
            n_lo / n_lo.hypot(w_hi)
        };
        let hi = if n_hi == 0. {
            0.
        } else {
            n_hi / n_hi.hypot(w_lo)
        };
        (lo, hi.min(1.))
    };
    let normal = range(face.normal);
    if normal.0 == 0. {
        // A grazing ray inside the cell: the lower bound is zero.
        return Ok(Some((0., f64::INFINITY)));
    }
    if face.vertical() {
        let axis = [face.normal[1], -face.normal[0], 0.];
        let width = range_along(&face.vertices, axis);
        let vertical = range([0., 0., 1.]);
        // |leg_xy|^2 = |D|^2 - leg_z^2.
        let xy_lo = (d_lo * d_lo - vertical.1 * vertical.1).max(0.).sqrt();
        let xy_hi = (d_hi * d_hi - vertical.0 * vertical.0).max(0.).sqrt();
        let horizontal = (
            normal.0 / xy_hi,
            if xy_lo == 0. {
                1.
            } else {
                (normal.1 / xy_lo).min(1.)
            },
        );
        let elevation = cosine(normal, vertical);
        let contacts: Vec<_> = paths.iter().map(|p| p[index + 1]).collect();
        let Some((h_lo, h_hi)) = contact_height_range(scene, face, &contacts, axis)? else {
            return Ok(None);
        };
        Ok(Some((
            (width * horizontal.0).min(h_lo * elevation.0),
            (width * horizontal.1).min(h_hi * elevation.1),
        )))
    } else {
        let a = range_along(&face.projected, [1., 0., 0.]);
        let h = range_along(&face.projected, [0., 1., 0.]);
        let cu = cosine(normal, range(face.u));
        let cv = cosine(normal, range(face.v));
        Ok(Some(((a * cu.0).min(h * cv.0), (a * cu.1).min(h * cv.1))))
    }
}

/// Range of the reflecting height at contacts along a vertical face. Polygon
/// top/bottom and terrain are piecewise linear along the face line, with
/// breakpoints at face vertices and terrain mesh crossings.
fn contact_height_range(
    scene: &Scene,
    face: &Face,
    contacts: &[Point],
    axis: Point,
) -> Result<Option<(f64, f64)>, Diagnostic> {
    let key = |p: &Point| dot(*p, axis);
    let first = *contacts
        .iter()
        .min_by(|a, b| key(a).total_cmp(&key(b)))
        .unwrap();
    let last = *contacts
        .iter()
        .max_by(|a, b| key(a).total_cmp(&key(b)))
        .unwrap();
    let (x0, x1) = (key(&first), key(&last));
    let mut samples = vec![first, last];
    for q in &face.vertices {
        let x = key(q);
        if x > x0 && x < x1 {
            samples.push(lerp(first, last, (x - x0) / (x1 - x0)));
        }
    }
    if face.anchored && x1 > x0 {
        for (t, _) in scene.terrain.profile(first, last)? {
            samples.push(lerp(first, last, t));
        }
    }
    let mut lo = f64::INFINITY;
    let mut hi: f64 = 0.;
    for p in samples {
        let Some((bottom, top)) = path::vertical_interval(face, p) else {
            return Ok(None);
        };
        let bottom = if face.anchored {
            scene.terrain.elevation(p[0], p[1])?
        } else {
            bottom
        };
        let h = (top - bottom).max(0.);
        lo = lo.min(h);
        hi = hi.max(h);
    }
    Ok(Some((lo, hi)))
}
fn range_along(points: &[Point], axis: Point) -> f64 {
    points
        .iter()
        .map(|p| dot(*p, axis))
        .fold(f64::NEG_INFINITY, f64::max)
        - points
            .iter()
            .map(|p| dot(*p, axis))
            .fold(f64::INFINITY, f64::min)
}
fn boxes_overlap(a: &[Point], b: &[Point]) -> bool {
    (0..2).all(|k| {
        a.iter().map(|p| p[k]).fold(f64::INFINITY, f64::min)
            <= b.iter().map(|p| p[k]).fold(f64::NEG_INFINITY, f64::max) + GEOM_EPS
            && b.iter().map(|p| p[k]).fold(f64::INFINITY, f64::min)
                <= a.iter().map(|p| p[k]).fold(f64::NEG_INFINITY, f64::max) + GEOM_EPS
    })
}
fn sweep_meets_face(face: &Face, sweep: &[Point]) -> bool {
    let (min, max) = sweep
        .iter()
        .map(|p| face.signed(*p))
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), d| {
            (lo.min(d), hi.max(d))
        });
    if min >= -GEOM_EPS || max <= GEOM_EPS {
        return false;
    }
    let axes = if face.vertical() {
        vec![[face.normal[1], -face.normal[0], 0.]]
    } else {
        vec![face.u, face.v]
    };
    axes.into_iter().all(|axis| {
        let lo = sweep
            .iter()
            .map(|p| dot(*p, axis))
            .fold(f64::INFINITY, f64::min);
        let hi = sweep
            .iter()
            .map(|p| dot(*p, axis))
            .fold(f64::NEG_INFINITY, f64::max);
        let flo = face
            .vertices
            .iter()
            .map(|p| dot(*p, axis))
            .fold(f64::INFINITY, f64::min);
        let fhi = face
            .vertices
            .iter()
            .map(|p| dot(*p, axis))
            .fold(f64::NEG_INFINITY, f64::max);
        lo <= fhi + GEOM_EPS && hi >= flo - GEOM_EPS
    })
}
fn terrain_clear(scene: &Scene, sweep: &[Point]) -> Result<bool, Diagnostic> {
    // Without vertices the surface is the plane, which `highest` returns.
    let top = scene.terrain.highest();
    if sweep.iter().all(|p| p[2] >= top - GEOM_EPS) {
        return Ok(true);
    }
    if !scene.terrain.is_planar() {
        return Ok(false);
    }
    let vertices = &scene.terrain.vertices;
    if vertices.len() < 2 {
        return Ok(false);
    }
    if scene.terrain.triangles.is_empty() {
        let a = vertices[0];
        let b = *vertices.last().unwrap();
        let d = sub(b, a);
        let den = d[0] * d[0] + d[1] * d[1];
        let ts: Vec<_> = sweep
            .iter()
            .map(|p| ((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / den)
            .collect();
        if !(ts.iter().all(|t| *t <= 0.)
            || ts.iter().all(|t| *t >= 1.)
            || ts.iter().all(|t| *t >= 0. && *t <= 1.))
        {
            return Ok(false);
        }
    } else {
        let hull = scene.terrain.xy_hull(convex_hull_xy);
        if !sweep.iter().all(|p| point_in_polygon_xy(*p, hull)) {
            return Ok(false);
        }
    }
    for p in sweep {
        if p[2] < scene.terrain.elevation(p[0], p[1])? - GEOM_EPS {
            return Ok(false);
        }
    }
    Ok(true)
}
fn convex_hull_xy(points: &[Point]) -> Vec<Point> {
    let mut p = points.to_vec();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    p.dedup_by(|a, b| a[0] == b[0] && a[1] == b[1]);
    if p.len() < 3 {
        return p;
    }
    let mut lo: Vec<Point> = Vec::new();
    for &q in &p {
        while lo.len() >= 2 && orient(lo[lo.len() - 2], lo[lo.len() - 1], q) <= 0. {
            lo.pop();
        }
        lo.push(q);
    }
    let mut hi: Vec<Point> = Vec::new();
    for &q in p.iter().rev() {
        while hi.len() >= 2 && orient(hi[hi.len() - 2], hi[hi.len() - 1], q) <= 0. {
            hi.pop();
        }
        hi.push(q);
    }
    lo.pop();
    hi.pop();
    lo.extend(hi);
    lo
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plane_partition_preserves_actual_sloped_area() {
        let t = Cell::Triangle([[0., 0., 0.], [4., 0., 4.], [0., 4., 0.]]);
        let p = Plane {
            normal: [1., 0., 0.],
            offset: 1.,
        };
        let parts = split_plane(t, p).unwrap();
        assert!(parts.len() >= 2);
        let area: f64 = parts.iter().map(Cell::measure).sum();
        assert!((area - 8. * 2f64.sqrt()).abs() < 1e-12);
    }
    #[test]
    fn distance_uses_entire_domain() {
        let t = Cell::Triangle([[0., 0., 0.], [4., 0., 0.], [0., 4., 0.]]);
        assert_eq!(minimum_distance(t, [1., 1., 3.]), 3.);
        assert_eq!(
            minimum_distance(Cell::Line([[0., 0., 0.], [10., 0., 0.]]), [5., 2., 0.]),
            2.
        );
    }
}
