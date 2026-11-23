//! Parallel in-memory execution with deterministic, exact reductions.
//!
//! A calculation is divided into work items: one source, one block of
//! receivers and one stage. An item walks the source's reflection tree node by
//! node, evaluating each node's receiver stubs as they are produced, so memory
//! follows the item rather than the source's total stub count. Every
//! source-receiver pair lies in exactly one item, and all sums are exact, so
//! without a pruning strategy results do not depend on how items are
//! scheduled or where they run. A strategy's live cutoffs depend on which
//! items have completed when an item starts.
use crate::{
    acoustics,
    beam::{self, Node, ReceiverBlock, ReceiverStub, SearchIndex},
    path::*,
    scene::*,
    types::*,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
// Progress path counts are calculations: one per receiver connection of a
// source stub or reflection beam (a source-receiver-family stub), pruned when
// it kept no energy and has omitted energy. Each lies in exactly one work
// item, so the counts do not depend on how the work is divided. Octave bands
// and quadrature samples are not counted separately.
struct PathCounter<'a> {
    state: Mutex<(usize, usize, std::time::Instant)>,
    report: &'a (dyn Fn(usize, usize) + Sync),
    geometry_rejected: AtomicUsize,
    bounded_bands: AtomicUsize,
    receiver_candidates: AtomicUsize,
    reflector_candidates: AtomicUsize,
    prefix_pruned: AtomicUsize,
    receiver_connections: AtomicUsize,
    empty_beams: AtomicUsize,
    receiver_stubs_pruned: AtomicUsize,
    /// Path diagnostic records produced so far, limited as they are produced
    /// so that no work item can hold more than the limit in memory.
    ledgers: AtomicUsize,
    /// Work operations, reported as the result's `operations`.
    operations: AtomicUsize,
}
const LEDGER_LIMIT: usize = 100_000;
/// The counts of a `PathCounter`, as one node returns them for its items.
/// They are integers, so their totals do not depend on where items ran.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PathStats {
    checked: usize,
    pruned: usize,
    geometry_rejected: usize,
    bounded_bands: usize,
    receiver_candidates: usize,
    reflector_candidates: usize,
    prefix_pruned: usize,
    receiver_connections: usize,
    empty_beams: usize,
    receiver_stubs_pruned: usize,
    ledgers: usize,
    operations: usize,
}
impl<'a> PathCounter<'a> {
    fn new(report: &'a (dyn Fn(usize, usize) + Sync)) -> Self {
        Self {
            state: Mutex::new((0, 0, std::time::Instant::now())),
            report,
            geometry_rejected: AtomicUsize::new(0),
            bounded_bands: AtomicUsize::new(0),
            receiver_candidates: AtomicUsize::new(0),
            reflector_candidates: AtomicUsize::new(0),
            prefix_pruned: AtomicUsize::new(0),
            receiver_connections: AtomicUsize::new(0),
            empty_beams: AtomicUsize::new(0),
            receiver_stubs_pruned: AtomicUsize::new(0),
            ledgers: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
        }
    }
    fn stats(&self) -> PathStats {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let load = |a: &AtomicUsize| a.load(Ordering::Relaxed);
        PathStats {
            checked: state.0,
            pruned: state.1,
            geometry_rejected: load(&self.geometry_rejected),
            bounded_bands: load(&self.bounded_bands),
            receiver_candidates: load(&self.receiver_candidates),
            reflector_candidates: load(&self.reflector_candidates),
            prefix_pruned: load(&self.prefix_pruned),
            receiver_connections: load(&self.receiver_connections),
            empty_beams: load(&self.empty_beams),
            receiver_stubs_pruned: load(&self.receiver_stubs_pruned),
            ledgers: load(&self.ledgers),
            operations: load(&self.operations),
        }
    }
    /// Add the counts of items counted elsewhere.
    fn absorb(&self, s: &PathStats) -> Result<(), Diagnostic> {
        for (counter, n) in [
            (&self.geometry_rejected, s.geometry_rejected),
            (&self.bounded_bands, s.bounded_bands),
            (&self.receiver_candidates, s.receiver_candidates),
            (&self.reflector_candidates, s.reflector_candidates),
            (&self.prefix_pruned, s.prefix_pruned),
            (&self.receiver_connections, s.receiver_connections),
            (&self.empty_beams, s.empty_beams),
            (&self.receiver_stubs_pruned, s.receiver_stubs_pruned),
            (&self.operations, s.operations),
        ] {
            counter.fetch_add(n, Ordering::Relaxed);
        }
        self.count_ledgers(s.ledgers)?;
        if s.checked + s.pruned > 0 {
            self.advance(s.checked, s.pruned);
        }
        Ok(())
    }
    fn count_ledgers(&self, records: usize) -> Result<(), Diagnostic> {
        if records > 0
            && self.ledgers.fetch_add(records, Ordering::Relaxed) + records > LEDGER_LIMIT
        {
            return Err(Diagnostic::new(
                "calculation_failed",
                "Requested path diagnostics exceed 100000 records; no truncated ledger is returned",
            ));
        }
        Ok(())
    }
    /// Count one family's receiver connections and advance the progress
    /// counts by them.
    fn record(&self, contributions: &[(usize, Contribution)]) {
        let bounded = contributions.iter().map(|(_, c)| c.bounded_bands).sum();
        self.bounded_bands.fetch_add(bounded, Ordering::Relaxed);
        self.receiver_connections
            .fetch_add(contributions.len(), Ordering::Relaxed);
        let pruned = contributions
            .iter()
            .filter(|(_, c)| c.omitted.iter().any(|e| *e > 0.) && !c.energy.iter().any(|e| *e > 0.))
            .count();
        self.receiver_stubs_pruned
            .fetch_add(pruned, Ordering::Relaxed);
        self.advance(contributions.len(), pruned);
    }
    fn advance(&self, checked: usize, pruned: usize) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.0 += checked;
        state.1 += pruned;
        if state.2.elapsed() >= std::time::Duration::from_millis(100) {
            (self.report)(state.0, state.1);
            state.2 = std::time::Instant::now();
        }
    }
    fn flush(&self) -> Value {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (self.report)(state.0, state.1);
        json!({"checked":state.0,"pruned":state.1})
    }
}
#[derive(Clone, Default)]
struct SourceTotal {
    energy: [Exact; 8],
    omitted: [Exact; 8],
    incomplete: bool,
}
/// Constant added to 10 log10(source count) to give the pruning margin. The
/// margin scales with the number of sources because skipped contributions
/// below a receiver's threshold accumulate roughly in proportion to it. Each
/// receiver is judged against its own level, so the constant sets the loss at
/// every receiver, not only at the quietest. Both values kept the weighted
/// total within 0.05 dB on two recorded scenes; dBA lost up to 0.0564 dB on a
/// third, so neither is a bound. dBC needs more: its total is set by the loud
/// low bands, so the same margin leaves the higher bands' thresholds relatively
/// higher than under dBA.
pub fn pruning_constant(strategy: &str) -> f64 {
    match strategy {
        "dbc" => 25.,
        _ => 20.,
    }
}

fn strategy_weights(scene: &Scene) -> [f64; 8] {
    match scene.settings.strategy.as_deref() {
        Some("dbc") => acoustics::C_WEIGHTING,
        _ => acoustics::A_WEIGHTING,
    }
}
/// A receiver's weighted threshold under a pruning strategy: its weighted
/// level from the contributions completed so far, less the margin. A band
/// contribution at that receiver is skipped when, once weighted, its upper
/// bound is below the threshold. A receiver with no energy yet has no
/// threshold and is not pruned.
fn receiver_threshold(weights: &[f64; 8], margin: f64, energy: &Bands) -> Option<f64> {
    let mut weighted = Sum::default();
    for b in 0..8 {
        weighted.add(energy[b] * 10f64.powf(weights[b] / 10.));
    }
    let weighted = weighted.total();
    (weighted > 0.).then(|| 10. * weighted.log10() - margin)
}
/// The cutoffs a work item starts with. A fixed cutoff applies to every
/// receiver. Under a pruning strategy each receiver of the item's block has
/// its own weighted threshold (see `receiver_threshold`), so a contribution
/// is judged against the receiver it reaches, never against a quieter one
/// elsewhere in the model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemCutoffs {
    /// Per-band cutoffs applied at every receiver.
    pub fixed: Cutoffs,
    /// Weighted thresholds of the block's receivers, in block order.
    pub thresholds: Option<Vec<Option<f64>>>,
    /// What a strategy's direct item does at each receiver of its block, in
    /// block order: `SKIP`, `EVALUATE` or `OMIT`.
    pub direct: Option<Vec<u8>>,
}
/// A direct item leaves this receiver to the other direct stage.
pub const SKIP: u8 = 0;
/// A direct item evaluates this receiver, unpruned.
pub const EVALUATE: u8 = 1;
/// A direct item reports the source's bound at this receiver as omitted
/// energy and does not evaluate it: the source is ranked out there.
pub const OMIT: u8 = 2;
impl ItemCutoffs {
    pub fn fixed(fixed: Cutoffs) -> Self {
        Self {
            fixed,
            ..Self::default()
        }
    }
    /// What a direct item does at the `k`-th receiver of its block.
    fn action(&self, k: usize) -> u8 {
        match &self.direct {
            None => EVALUATE,
            Some(actions) => actions.get(k).copied().unwrap_or(SKIP),
        }
    }
    /// Whether these cutoffs fit an item of `stage` over a block of
    /// `receivers`. A strategy's direct items take one action per receiver
    /// and nothing else: without the list both direct stages would evaluate
    /// every receiver. Receiver thresholds belong to a strategy's reflection
    /// items only, one finite value or none per receiver. Neither list
    /// accompanies fixed cutoffs. A list that fits no such item is rejected,
    /// not applied.
    fn check(&self, stage: Stage, receivers: usize) -> Result<(), Diagnostic> {
        let length = |what: &str, n: usize| {
            (n != receivers)
                .then(|| format!("{n} receiver {what} for a block of {receivers} receivers"))
        };
        let problem = match (&self.direct, &self.thresholds) {
            (Some(_), Some(_)) => Some("both receiver actions and receiver thresholds".into()),
            (Some(actions), None) => {
                if !stage.direct() {
                    Some("receiver actions for an item that is not a strategy's direct item".into())
                } else if pruning(&self.fixed) {
                    Some("receiver actions together with fixed cutoffs".into())
                } else if let Some(problem) = length("actions", actions.len()) {
                    Some(problem)
                } else if actions
                    .iter()
                    .any(|a| *a > OMIT || (stage == Stage::Lead && *a == OMIT))
                {
                    Some("a receiver action that does not fit the item".into())
                } else {
                    None
                }
            }
            (None, Some(thresholds)) => {
                if !matches!(stage, Stage::Order(_)) {
                    Some(
                        "receiver thresholds for an item that is not a strategy's reflection item"
                            .into(),
                    )
                } else if pruning(&self.fixed) {
                    Some("receiver thresholds together with fixed cutoffs".into())
                } else if let Some(problem) = length("thresholds", thresholds.len()) {
                    Some(problem)
                } else if thresholds.iter().flatten().any(|t| !t.is_finite()) {
                    Some("a receiver threshold that is not finite".into())
                } else {
                    None
                }
            }
            (None, None) if stage.direct() => {
                Some("no receiver actions for a strategy's direct item".into())
            }
            (None, None) => None,
        };
        match problem {
            Some(problem) => Err(Diagnostic::invalid(format!(
                "Work item input has {problem}"
            ))),
            None => Ok(()),
        }
    }
    /// Band cutoffs at the `k`-th receiver of the block.
    fn at(&self, weights: &[f64; 8], k: usize) -> Cutoffs {
        match &self.thresholds {
            None => self.fixed,
            Some(thresholds) => match thresholds.get(k).copied().flatten() {
                Some(t) => std::array::from_fn(|b| Some(t - weights[b])),
                None => [None; 8],
            },
        }
    }
    /// Band cutoffs no higher than any receiver's, for decisions shared by
    /// the whole block (reflection prefixes). None when any receiver of the
    /// block has no threshold: nothing may be cut on its behalf.
    fn floor(&self, weights: &[f64; 8]) -> Cutoffs {
        match &self.thresholds {
            None => self.fixed,
            Some(thresholds) => {
                let mut lowest = f64::INFINITY;
                for t in thresholds {
                    match t {
                        Some(t) => lowest = lowest.min(*t),
                        None => return [None; 8],
                    }
                }
                if lowest.is_finite() {
                    std::array::from_fn(|b| Some(lowest - weights[b]))
                } else {
                    [None; 8]
                }
            }
        }
    }
}
/// A stage of the work plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Every reflection order in one item: no pruning strategy.
    All,
    /// Direct paths from each receiver's leading sources.
    Lead,
    /// Direct paths from each receiver's other sources.
    Rest,
    /// Families with exactly this many reflections, at least one.
    Order(usize),
}
impl Stage {
    /// The one reflection order the stage connects, if it is limited to one.
    fn order(self) -> Option<usize> {
        match self {
            Stage::All => None,
            Stage::Lead | Stage::Rest => Some(0),
            Stage::Order(k) => Some(k),
        }
    }
    fn direct(self) -> bool {
        matches!(self, Stage::Lead | Stage::Rest)
    }
}

/// Ranking of the sources at each receiver under a pruning strategy, by a
/// conservative bound on what each can deliver along the direct family.
///
/// The bound has the terms of the reflection prefix bound: whole-source
/// power, maximum directivity, a +6 dB net ground allowance, and divergence
/// and air absorption over the shortest distance from the receiver to the
/// source's bounding box. Screening, meteorology and miscellaneous losses are
/// taken as zero. The direct family never exceeds it, because the family is
/// capped at its unscreened reference.
///
/// At a receiver, the sources whose weighted bound is within the strategy's
/// margin constant c of the largest are its leaders: none of them can be
/// ranked out, since the bounds of everything ranked out sum to at most
/// 10^(-c/10) of the leaders' energy and that energy is no more than the
/// leaders' bounds. Leaders are evaluated first; the other sources are then
/// ranked against what the leaders delivered (see `rest_floor`).
struct Ranking {
    /// Per source: its bounding box and, per band, its power times the
    /// directivity and ground allowance and the constant of divergence.
    sources: Vec<(Point, Point, Bands)>,
    /// Linear strategy weighting of each band.
    weights: Bands,
    /// 10^(-c/10).
    fraction: f64,
    /// Per receiver: the lowest weighted bound of a leader.
    lead_floor: Vec<f64>,
    /// Per receiver: its number of leaders.
    leaders: Vec<u32>,
}
impl Ranking {
    fn new(scene: &Scene, cancel: &AtomicBool) -> Result<Self, Diagnostic> {
        let strategy = scene.settings.strategy.as_deref().unwrap_or("dba");
        let sources = scene
            .sources
            .iter()
            .map(|source| {
                let mut low = [f64::INFINITY; 3];
                let mut high = [f64::NEG_INFINITY; 3];
                for cell in source.zones.iter().flat_map(|z| &z.cells) {
                    for p in cell.vertices() {
                        for k in 0..3 {
                            low[k] = low[k].min(p[k]);
                            high[k] = high[k].max(p[k]);
                        }
                    }
                }
                let gain = std::array::from_fn(|b| {
                    let power: f64 = source.zones.iter().map(|z| z.power[b]).sum();
                    let directivity = source
                        .zones
                        .iter()
                        .map(|z| {
                            z.directivity
                                .as_ref()
                                .map_or(0., |d| d.maximum_correction()[b])
                        })
                        .fold(f64::NEG_INFINITY, f64::max);
                    if power > 0. {
                        power * 10f64.powf((directivity + 6. - 11.) / 10.)
                    } else {
                        0.
                    }
                });
                (low, high, gain)
            })
            .collect();
        let mut ranking = Self {
            sources,
            weights: strategy_weights(scene).map(|w| 10f64.powf(w / 10.)),
            fraction: 10f64.powf(-pruning_constant(strategy) / 10.),
            lead_floor: vec![],
            leaders: vec![],
        };
        let floors: Result<Vec<(f64, u32)>, Diagnostic> = (0..scene.receivers.len())
            .into_par_iter()
            .map(|ri| {
                check_cancel(cancel)?;
                let bounds: Vec<f64> = (0..scene.sources.len())
                    .map(|si| ranking.weighted(scene, si, ri))
                    .collect();
                let floor = bounds.iter().copied().fold(0., f64::max) * ranking.fraction;
                let leaders = bounds.iter().filter(|u| **u > 0. && **u >= floor).count();
                Ok((floor, leaders as u32))
            })
            .collect();
        (ranking.lead_floor, ranking.leaders) = floors?.into_iter().unzip();
        Ok(ranking)
    }
    /// Upper bound of the direct family's energy from source `si` at
    /// receiver `ri`, per band.
    fn bound(&self, scene: &Scene, si: usize, ri: usize) -> Bands {
        let (low, high, gain) = &self.sources[si];
        let p = scene.receivers[ri].point;
        let squared: f64 = (0..3)
            .map(|k| (low[k] - p[k]).max(p[k] - high[k]).max(0.).powi(2))
            .sum();
        let scale = low
            .iter()
            .chain(high)
            .chain(&p)
            .map(|x| x.abs())
            .fold(1., f64::max);
        let reach = squared.sqrt() - 1e-9 * scale;
        std::array::from_fn(|b| {
            if gain[b] <= 0. {
                0.
            } else if reach <= 0. {
                f64::INFINITY
            } else {
                gain[b] / (reach * reach) * 10f64.powf(-scene.alpha[b] * reach / 10.) * (1. + 1e-12)
            }
        })
    }
    /// The strategy-weighted sum of `bound` over the bands.
    fn weighted(&self, scene: &Scene, si: usize, ri: usize) -> f64 {
        let bound = self.bound(scene, si, ri);
        (0..8).map(|b| bound[b] * self.weights[b]).sum()
    }
    /// Whether source `si` is a leader at receiver `ri`.
    fn leads(&self, scene: &Scene, si: usize, ri: usize) -> bool {
        let bound = self.weighted(scene, si, ri);
        bound > 0. && bound >= self.lead_floor[ri]
    }
    /// The lowest weighted bound still evaluated at receiver `ri` once its
    /// leaders have delivered the energy `lead`: the other sources are taken
    /// from the lowest weighted bound upwards, and ranked out while, in every
    /// band, their bounds sum to at most 10^(-c/10) of `lead` in that band. The
    /// weighted sum then keeps the same share. A source is ranked out when
    /// its weighted bound is below the value returned; equal bounds are kept.
    fn rest_floor(&self, scene: &Scene, ri: usize, lead: &Bands) -> f64 {
        let mut rest: Vec<(f64, Bands)> = (0..scene.sources.len())
            .map(|si| {
                let bound = self.bound(scene, si, ri);
                ((0..8).map(|b| bound[b] * self.weights[b]).sum(), bound)
            })
            .filter(|(u, _)| *u > 0. && *u < self.lead_floor[ri])
            .collect();
        rest.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let mut sum = [0.; 8];
        for (weighted, bound) in rest {
            for b in 0..8 {
                sum[b] += bound[b];
                if sum[b] > lead[b] * self.fraction {
                    return weighted;
                }
            }
        }
        f64::INFINITY
    }
}

/// Merge-side state of the ranked direct stages.
#[derive(Default)]
struct Ranked {
    /// Energy each receiver has from its leaders, per band.
    lead: Vec<[Exact; 8]>,
    /// Leaders of each receiver whose lead item has not been merged.
    pending: Vec<u32>,
    /// Once a receiver's leaders are merged: `Ranking::rest_floor` there.
    rest_floor: Vec<Option<f64>>,
    /// Lead items merged; each is counted once.
    merged: Vec<bool>,
    /// Per direct item, from its latest input: an item whose input is taken
    /// again (a retry) is counted once.
    tallies: Vec<Tally>,
}
/// What a direct item's input decided.
#[derive(Clone, Copy, Default)]
struct Tally {
    /// Source-receiver pairs: leading, evaluated after ranking, ranked out,
    /// and evaluated because the receiver's leaders were not all merged.
    pairs: [usize; 4],
    /// A source's first remaining-source item that ranked no receiver,
    /// because leaders were still to be merged.
    unranked: bool,
}
impl Ranked {
    fn pairs(&self) -> [usize; 4] {
        std::array::from_fn(|k| self.tallies.iter().map(|t| t.pairs[k]).sum())
    }
    fn unranked(&self) -> usize {
        self.tallies.iter().filter(|t| t.unranked).count()
    }
}

/// Live state of a pruning strategy. Contributions only add energy, so each
/// receiver's level, and with it its threshold, rises as items complete; an
/// item started earlier uses lower, more conservative thresholds. No
/// threshold applies until every direct item has completed.
struct Live {
    direct_pending: usize,
    /// Per reflection order: sources started without any threshold, and the
    /// lowest and highest receiver threshold applied.
    used: Vec<(usize, f64, f64)>,
}
pub fn pruning_margin(strategy: &str, sources: usize) -> f64 {
    10. * (sources.max(1) as f64).log10() + pruning_constant(strategy)
}
/// Bands are carried as bit patterns between machines, so every value,
/// non-finite ones included, arrives exactly.
#[derive(Default, Serialize, Deserialize)]
struct Contribution {
    #[serde(with = "band_bits")]
    energy: Bands,
    #[serde(with = "band_bits")]
    omitted: Bands,
    ledgers: Vec<Value>,
    reports: Vec<Value>,
    bounded_bands: usize,
    /// Extended-source boundary allowance deferred to the source-receiver
    /// tolerance, with the first deferring family's diagnostic.
    #[serde(with = "band_bits")]
    unresolved: Bands,
    unresolved_message: Option<String>,
    /// Point-family energy an incomplete joint contact search may have
    /// missed (family cap minus found routes), with its first diagnostic.
    /// Reported as omitted energy and limited by the receiver-total tolerance.
    #[serde(with = "band_bits")]
    contact_unresolved: Bands,
    contact_message: Option<String>,
    /// Pathway-level problems that did not stop the run (see `pathway_error`).
    warnings: Vec<Value>,
    /// A pathway failed without an energy bound; levels may be understated.
    incomplete: bool,
    /// A stub's zone integrations; pairs and items carry them separately.
    #[serde(skip)]
    integration: crate::integration::Summary,
}
/// Errors confined to one source/receiver/reflection-family transfer. The run
/// continues and reports them; all other diagnostics stop the calculation.
pub(crate) fn pathway_error(code: &str) -> bool {
    matches!(
        code,
        "calculation_failed" | "nonfinite_result" | crate::path::joint_contact::INCOMPLETE
    )
}
fn fields(energy: Bands, omitted: Bands) -> Result<Value, Diagnostic> {
    let mut levels = [None; 8];
    let mut states = vec![];
    let mut increases = [None; 8];
    let mut upper = [None; 8];
    for b in 0..8 {
        if !energy[b].is_finite() || energy[b] < 0. || !omitted[b].is_finite() || omitted[b] < 0. {
            return Err(Diagnostic::numerical("Energy reduction overflow"));
        }
        levels[b] = acoustics::level_from_energy(energy[b])?;
        states.push(if energy[b] > 0. {
            "calculated"
        } else if omitted[b] > 0. {
            "cutoff_limited"
        } else {
            "exact_zero"
        });
        increases[b] = if energy[b] > 0. {
            Some(10. * (omitted[b] / energy[b]).ln_1p() / std::f64::consts::LN_10)
        } else if omitted[b] == 0. {
            Some(0.)
        } else {
            None
        };
        if omitted[b] > 0. {
            upper[b] = acoustics::level_from_energy(energy[b] + omitted[b])?
        }
    }
    let state = if energy.iter().any(|e| *e > 0.) {
        "calculated"
    } else if omitted.iter().any(|e| *e > 0.) {
        "cutoff_limited"
    } else {
        "exact_zero"
    };
    let la = acoustics::level_from_energy(acoustics::a_weighted_energy(energy)?)?;
    Ok(
        json!({"octave_db":levels,"band_states":states,"a_weighted_db":la,"state":state,"omitted_energy":omitted,"omission_increase_db":increases,"omission_upper_db":upper}),
    )
}
fn calculate_receiver_stub(
    scene: &Scene,
    source: &Source,
    family: &Family,
    stub: ReceiverStub,
    cutoffs: &Cutoffs,
    work: &Work,
) -> Result<(usize, Contribution), Diagnostic> {
    work.tick()?;
    let receiver = &scene.receivers[stub.receiver];
    let mut out = Contribution::default();
    if source.kind == "point" {
        let zone = &source.zones[0];
        let s = zone.cells[0].centroid();
        let prepared = stub
            .reference
            .expect("point connection is prepared by beam search");
        let t = transfer_prepared(
            scene, source, zone, s, receiver, family, prepared, true, cutoffs, work,
        )?;
        for b in 0..8 {
            out.energy[b] = t.energy[b] * zone.power[b];
            if t.energy[b] > 0. && zone.power[b] > 0. {
                positive(out.energy[b], "Received point energy")?;
            }
        }
        out.omitted = t.omitted;
        out.bounded_bands = t.omitted.iter().filter(|e| **e > 0.).count();
        out.ledgers = t.ledgers;
        if let Some(message) = t.unresolved_message {
            for b in 0..8 {
                out.contact_unresolved[b] = t.unresolved[b] * zone.power[b];
            }
            out.contact_message = Some(format!(
                "Source {}, receiver {}, reflection faces {:?}: {}",
                source.id,
                receiver.id,
                family
                    .iter()
                    .map(|i| scene.faces[*i].key())
                    .collect::<Vec<_>>(),
                message
            ));
        }
    } else {
        // Prune only on a conservative bound, before integration; a computed
        // contribution is always kept.
        let mut pruned = [false; 8];
        if pruning(cutoffs) {
            let bound = crate::integration::family_bound(scene, source, receiver, family)?;
            for b in 0..8 {
                if below_cutoff(cutoffs, b, bound[b]) {
                    pruned[b] = true;
                    out.omitted[b] = bound[b];
                }
            }
            out.bounded_bands = pruned.iter().filter(|p| **p).count();
        }
        let live = (0..8).any(|b| !pruned[b] && source.zones.iter().any(|z| z.power[b] > 0.));
        if !live && pruned.iter().any(|p| *p) {
            return Ok((stub.receiver, out));
        }
        let integrated =
            crate::integration::integrate(scene, source, receiver, family, pruned, work)?;
        out.energy = integrated.energy;
        out.ledgers = integrated.ledgers;
        for report in &integrated.reports {
            out.integration.add(source, stub.receiver, family, report);
        }
        if string(&scene.output, "integration_reports", "summary") == "full" {
            out.reports = integrated
                .reports
                .iter()
                .map(|r| r.json(scene, source, receiver, family))
                .collect();
        }
        if let Some(u) = integrated.unresolved {
            out.unresolved = u.allowance;
            out.unresolved_message = Some(format!(
                "Source {}, receiver {}, reflection faces {:?}: {}",
                source.id,
                receiver.id,
                family
                    .iter()
                    .map(|i| scene.faces[*i].key())
                    .collect::<Vec<_>>(),
                u.message
            ));
        }
    }
    if string(&scene.output, "path_diagnostics", "none") != "none" {
        out.ledgers
            .push(json!({"receiver_id":receiver.id,"source_id":source.id,
            "reflection_faces":family.iter().map(|i|scene.faces[*i].key()).collect::<Vec<_>>(),
            "completed_family_energy":out.energy,"omitted_energy":out.omitted,
            "omission_basis":"conservative family upper bound evaluated before path construction",
            "bounded_pruned_bands":out.bounded_bands,
            "cutoff_scope":"complete original source and reflection family; all zones"}));
    }
    Ok((stub.receiver, out))
}

/// Receiver blocks of the work plan; each (source, block) pair is one work
/// item per stage. Every block re-traces its source's reflection tree, so the
/// count is one when sources alone give enough items, and grows only for
/// models with few sources. Point and extended sources are divided
/// separately, each by its own source count and smallest block size (see
/// `min_block_receivers`). The division depends on the model alone, never on
/// thread or node counts, so diagnostics are identical however the items are
/// run. Blocks never exceed `MAX_BLOCK_RECEIVERS`, which bounds the per-task
/// accumulators of an item.
const TARGET_ITEMS: usize = 1024;
const MAX_BLOCK_RECEIVERS: usize = 4096;
/// Smallest receiver block for a source kind. A point source's stubs are
/// cheap, so re-tracing it for every block is a visible cost: 16-receiver
/// blocks rather than 1 kept a model of three point sources over a 20 m grid
/// at order 3 at 12.8 s instead of 14.5 s. A line or area source's stubs are
/// integrations, beside which re-tracing is negligible (the road model took
/// 110-111 s for any block size). One-receiver blocks keep its items small
/// enough to balance over unequal nodes: 111-113 s on nodes of 3, 2 and 1
/// cores, against 130-137 s with 16-receiver blocks.
fn min_block_receivers(kind: &str) -> usize {
    if kind == "point" { 16 } else { 1 }
}
fn receiver_blocks(
    sources: usize,
    receivers: usize,
    target_items: usize,
    min_block: usize,
) -> Vec<std::ops::Range<usize>> {
    let most = receivers.div_ceil(min_block.max(1)).max(1);
    let blocks = target_items
        .div_ceil(sources.max(1))
        .clamp(1, most)
        .max(receivers.div_ceil(MAX_BLOCK_RECEIVERS));
    (0..blocks)
        .map(|k| k * receivers / blocks..(k + 1) * receivers / blocks)
        .collect()
}

/// A family's ledgers, reports and warnings at one receiver.
type FamilyRecords = (Family, Vec<Value>, Vec<Value>, Vec<Value>);
/// One source-receiver transfer, accumulated over families in any order.
/// Lists are emitted, and first messages chosen, in family order.
#[derive(Default)]
struct Pair {
    energy: [Exact; 8],
    omitted: [Exact; 8],
    unresolved: [Exact; 8],
    contact: [Exact; 8],
    incomplete: bool,
    /// Ledgers, reports and warnings of each family that produced any.
    records: Vec<FamilyRecords>,
    unresolved_message: Option<(Family, String)>,
    contact_message: Option<(Family, String)>,
    integration: crate::integration::Summary,
}
fn earliest(slot: &mut Option<(Family, String)>, family: &Family, message: Option<String>) {
    if let Some(message) = message
        && slot
            .as_ref()
            .is_none_or(|(first, _)| beam::family_order(family, first).is_lt())
    {
        *slot = Some((family.clone(), message));
    }
}
impl Pair {
    fn merge(&mut self, other: Pair) {
        for b in 0..8 {
            self.energy[b].merge(&other.energy[b]);
            self.omitted[b].merge(&other.omitted[b]);
            self.unresolved[b].merge(&other.unresolved[b]);
            self.contact[b].merge(&other.contact[b]);
        }
        self.incomplete |= other.incomplete;
        for (slot, theirs) in [
            (&mut self.unresolved_message, other.unresolved_message),
            (&mut self.contact_message, other.contact_message),
        ] {
            if let Some((family, message)) = theirs {
                earliest(slot, &family, Some(message));
            }
        }
        self.records.extend(other.records);
        self.integration.merge(other.integration);
    }
    fn add(&mut self, family: &Family, c: Contribution) {
        for b in 0..8 {
            self.energy[b].add(c.energy[b]);
            self.omitted[b].add(c.omitted[b]);
            self.unresolved[b].add(c.unresolved[b]);
            self.contact[b].add(c.contact_unresolved[b]);
        }
        self.incomplete |= c.incomplete;
        self.integration.merge(c.integration);
        earliest(&mut self.unresolved_message, family, c.unresolved_message);
        earliest(&mut self.contact_message, family, c.contact_message);
        if !(c.ledgers.is_empty() && c.reports.is_empty() && c.warnings.is_empty()) {
            self.records
                .push((family.clone(), c.ledgers, c.reports, c.warnings));
        }
    }
    /// The complete source-receiver transfer: every family of the pair is in
    /// this item, so the deferred allowance is checked against its full total.
    fn finish(
        mut self,
        scene: &Scene,
        source: &Source,
        ri: usize,
        integration: &mut crate::integration::Summary,
    ) -> Contribution {
        integration.merge(std::mem::take(&mut self.integration));
        self.records.sort_by(|a, b| beam::family_order(&a.0, &b.0));
        let mut out = Contribution::default();
        for (_, ledgers, reports, warnings) in self.records {
            out.ledgers.extend(ledgers);
            out.reports.extend(reports);
            out.warnings.extend(warnings);
        }
        out.energy = self.energy.each_ref().map(Exact::total);
        out.omitted = self.omitted.each_ref().map(Exact::total);
        out.contact_unresolved = self.contact.each_ref().map(Exact::total);
        out.incomplete = self.incomplete;
        out.contact_message = self.contact_message.map(|(_, message)| message);
        if let Some((_, message)) = self.unresolved_message {
            // The family-level interval test applied to the whole source-receiver
            // transfer: deferred allowances must fit the tolerance of the total
            // computed energy (cutoff omissions included) of every family.
            let allowance = self.unresolved.each_ref().map(Exact::total);
            let fits = (0..8).all(|b| {
                allowance[b].is_finite()
                    && allowance[b]
                        <= scene.settings.absolute
                            + scene.settings.relative * (out.energy[b] + out.omitted[b])
            });
            if !fits {
                // Bounded, but above tolerance: report it and keep the trial energy.
                out.warnings
                    .push(json!({"kind":"integration_tolerance_exceeded",
                    "source_id":source.id,"receiver_id":scene.receivers[ri].id,"bounded":true,
                    "allowance":allowance,"source_receiver_energy":out.energy,"message":message}));
            }
            out.reports.push(json!({"source_id":source.id,"receiver_id":scene.receivers[ri].id,
                "boundary_tolerance_scope":"source_receiver_total","deferred_boundary_allowance":allowance,
                "source_receiver_energy":out.energy,"source_receiver_omitted_energy":out.omitted,
                "absolute_tolerance":scene.settings.absolute,"relative_tolerance":scene.settings.relative}));
        }
        out
    }
}

/// Source-receiver pairs accumulated by one piece of an item's tree walk.
type Pairs = HashMap<usize, Pair>;
fn merge_pairs(mut into: Pairs, from: Pairs) -> Pairs {
    let (mut into, from) = if into.len() >= from.len() {
        (into, from)
    } else {
        (from, std::mem::take(&mut into))
    };
    for (ri, pair) in from {
        into.entry(ri).or_default().merge(pair);
    }
    into
}

/// The execution of one work item. The reflection tree is walked depth
/// first; each node's children are a parallel iterator that Rayon splits
/// only while workers are idle, so granularity adapts to the load. Every
/// split piece keeps its own pair totals and pieces merge as they finish:
/// no shared accumulator, and no list of the source's stubs, is ever built.
struct ItemRun<'a> {
    scene: &'a Scene,
    source: &'a Source,
    index: &'a SearchIndex,
    block: &'a ReceiverBlock,
    stage: Stage,
    ranking: Option<&'a Ranking>,
    cutoffs: &'a ItemCutoffs,
    /// Cutoffs valid for every receiver of the block (`ItemCutoffs::floor`).
    floor: Cutoffs,
    weights: [f64; 8],
    work: &'a Work<'a>,
    paths: &'a PathCounter<'a>,
    /// Tracing statistics are counted once per source, by its first block.
    first: bool,
}
impl ItemRun<'_> {
    fn subtree(&self, node: Node, pairs: &mut Pairs) -> Result<(), Diagnostic> {
        let mut children = self.step(&node, pairs)?;
        drop(node);
        match children.len() {
            0 => Ok(()),
            1 => self.subtree(children.pop().unwrap(), pairs),
            _ => {
                let found = children
                    .into_par_iter()
                    .try_fold(Pairs::new, |mut pairs, child| {
                        self.subtree(child, &mut pairs).map(|()| pairs)
                    })
                    .try_reduce(Pairs::new, |a, b| Ok(merge_pairs(a, b)))?;
                *pairs = merge_pairs(std::mem::take(pairs), found);
                Ok(())
            }
        }
    }
    /// Expand one node: connect and evaluate its receivers, return children.
    fn step(&self, node: &Node, pairs: &mut Pairs) -> Result<Vec<Node>, Diagnostic> {
        let start = self.block.range.start;
        // A direct item connects only the receivers it is to evaluate.
        let evaluated = |ri: usize| self.cutoffs.action(ri - start) == EVALUATE;
        let expansion = beam::expand(
            self.scene,
            self.source,
            self.index,
            self.block,
            node,
            self.stage.order(),
            &self.floor,
            self.stage.direct().then_some(&evaluated),
            self.work.cancel,
        )?;
        let (paths, stats) = (self.paths, &expansion.stats);
        paths
            .receiver_candidates
            .fetch_add(stats.receiver_candidates, Ordering::Relaxed);
        paths
            .geometry_rejected
            .fetch_add(stats.geometry_rejected, Ordering::Relaxed);
        if self.first {
            paths
                .reflector_candidates
                .fetch_add(stats.reflector_candidates, Ordering::Relaxed);
            paths
                .prefix_pruned
                .fetch_add(stats.prefix_pruned, Ordering::Relaxed);
            paths.empty_beams.fetch_add(stats.empty, Ordering::Relaxed);
        }
        let family = &node.family;
        let results: Result<Vec<_>, Diagnostic> = expansion
            .receiver_stubs
            .into_par_iter()
            .map(|stub| self.evaluate(family, stub))
            .collect();
        let mut results = results?;
        if self.stage == Stage::Rest && family.is_empty() {
            results.extend(self.ranked_out());
        }
        if !results.is_empty() {
            paths.record(&results);
            paths.count_ledgers(results.iter().map(|(_, c)| c.ledgers.len()).sum())?;
            for (ri, c) in results {
                pairs.entry(ri).or_default().add(family, c);
            }
        }
        Ok(expansion.children)
    }
    /// The receivers of the block where the source is ranked out: its direct
    /// family is not evaluated there, and its bound is the omitted energy.
    fn ranked_out(&self) -> Vec<(usize, Contribution)> {
        let (scene, source) = (self.scene, self.source);
        let Some(ranking) = self.ranking else {
            return vec![];
        };
        let si = scene
            .sources
            .iter()
            .position(|s| std::ptr::eq(s, source))
            .expect("the item's source is one of the scene's");
        let diagnostics = string(&scene.output, "path_diagnostics", "none") != "none";
        self.block
            .range
            .clone()
            .filter(|ri| self.cutoffs.action(ri - self.block.range.start) == OMIT)
            .map(|ri| {
                let mut out = Contribution {
                    omitted: ranking.bound(scene, si, ri),
                    ..Contribution::default()
                };
                out.bounded_bands = out.omitted.iter().filter(|e| **e > 0.).count();
                if diagnostics {
                    out.ledgers.push(json!({"receiver_id":scene.receivers[ri].id,"source_id":source.id,
                        "reflection_faces":[],
                        "completed_family_energy":out.energy,"omitted_energy":out.omitted,
                        "omission_basis":"ranked direct bound: source power, maximum directivity, +6 dB ground allowance, divergence and air absorption over the shortest distance to the source",
                        "bounded_pruned_bands":out.bounded_bands,
                        "cutoff_scope":"complete original source, direct family; all zones"}));
                }
                (ri, out)
            })
            .collect()
    }
    fn evaluate(
        &self,
        family: &Family,
        mut stub: ReceiverStub,
    ) -> Result<(usize, Contribution), Diagnostic> {
        let (scene, source) = (self.scene, self.source);
        let ri = stub.receiver;
        let receiver_id = &scene.receivers[ri].id;
        let faces = || {
            family
                .iter()
                .map(|i| scene.faces[*i].key())
                .collect::<Vec<_>>()
        };
        // A terminal calculation is synchronous. Count its hot-loop work
        // locally, then merge once, avoiding cache-line contention between
        // all workers on every geometry/search operation.
        let failure = stub.failure.take();
        let cutoffs = self.cutoffs.at(&self.weights, ri - self.block.range.start);
        match failure {
            Some(error) => Err(error),
            None => self.work.isolated(|work| {
                calculate_receiver_stub(scene, source, family, stub, &cutoffs, work)
            }),
        }
        .map_err(|mut error| {
            if error.code != "cancelled" {
                error.message = format!(
                    "Source {}, receiver {}, reflection faces {:?}: {}",
                    source.id,
                    receiver_id,
                    faces(),
                    error.message
                );
            }
            error
        })
        .or_else(|error| {
            if !pathway_error(&error.code) {
                return Err(error);
            }
            // Unbounded: this family's energy is missing at the receiver.
            Ok((
                ri,
                Contribution {
                    warnings: vec![json!({"kind":"failed_pathway","code":error.code,
                            "source_id":source.id,"receiver_id":receiver_id,
                            "reflection_faces":faces(),
                            "bounded":false,"message":error.message})],
                    incomplete: true,
                    ..Contribution::default()
                },
            ))
        })
    }
}

/// Evaluate one work item: every connection of `source` to the receivers of
/// `block` at one stage. Returns the item's source-receiver transfers in
/// receiver order. Items share no mutable state except diagnostic counters.
#[allow(clippy::too_many_arguments)]
fn trace_item(
    scene: &Scene,
    index: &SearchIndex,
    block: &ReceiverBlock,
    first: bool,
    source: &Source,
    stage: Stage,
    ranking: Option<&Ranking>,
    cutoffs: &ItemCutoffs,
    work: &Work,
    paths: &PathCounter,
) -> Result<(Vec<(usize, Contribution)>, crate::integration::Summary), Diagnostic> {
    check_cancel(work.cancel)?;
    cutoffs.check(stage, block.range.len())?;
    let weights = strategy_weights(scene);
    let run = ItemRun {
        scene,
        source,
        index,
        block,
        stage,
        ranking,
        cutoffs,
        floor: cutoffs.floor(&weights),
        weights,
        work,
        paths,
        first,
    };
    let mut pairs = Pairs::new();
    if !scene.receivers.is_empty() {
        run.subtree(Node::root(source), &mut pairs)?;
    }
    let mut pairs: Vec<(usize, Pair)> = pairs.into_iter().collect();
    pairs.sort_unstable_by_key(|(ri, _)| *ri);
    let mut integration = crate::integration::Summary::default();
    let contributions = pairs
        .into_iter()
        .map(|(ri, pair)| (ri, pair.finish(scene, source, ri, &mut integration)))
        .collect();
    Ok((contributions, integration))
}

/// Runs `task(i)` for every `i` in `0..count` on the current pool, in index
/// order, starting an index only when an earlier one finishes so that at most
/// `in_flight` are queued or running. A worker waiting inside a task runs
/// queued work on its own stack; bounding the queue bounds that nesting.
fn bounded<F: Fn(usize) + Sync>(count: usize, in_flight: usize, task: F) {
    fn launch<'s, F: Fn(usize) + Sync>(
        scope: &rayon::ScopeFifo<'s>,
        next: &'s AtomicUsize,
        count: usize,
        task: &'s F,
    ) {
        let i = next.fetch_add(1, Ordering::Relaxed);
        if i < count {
            scope.spawn_fifo(move |scope| {
                task(i);
                launch(scope, next, count, task);
            });
        }
    }
    let next = AtomicUsize::new(0);
    rayon::scope_fifo(|scope| {
        for _ in 0..in_flight.min(count) {
            launch(scope, &next, count, &task);
        }
    });
}

/// Receiver accumulators of a calculation. Sums are exact, so totals do not
/// depend on the order in which items complete; ordered lists carry their
/// item key (stage, then source) and are sorted once at the end.
struct Totals {
    energy: Vec<[Exact; 8]>,
    omitted: Vec<[Exact; 8]>,
    contact: Vec<[Exact; 8]>,
    contact_messages: Vec<Option<(usize, String)>>,
    warnings: Vec<Vec<(usize, Vec<Value>)>>,
    ledgers: Vec<Vec<(usize, Vec<Value>)>>,
    reports: Vec<Vec<(usize, Vec<Value>)>>,
    incomplete: Vec<bool>,
    /// Per-source totals of detailed receivers, summed over stages.
    sources: Vec<Vec<SourceTotal>>,
    integration: crate::integration::Summary,
}
impl Totals {
    fn new(receivers: usize, sources: usize, detailed: &[bool]) -> Self {
        Self {
            energy: vec![Default::default(); receivers],
            omitted: vec![Default::default(); receivers],
            contact: vec![Default::default(); receivers],
            contact_messages: vec![None; receivers],
            warnings: vec![vec![]; receivers],
            ledgers: vec![vec![]; receivers],
            reports: vec![vec![]; receivers],
            incomplete: vec![false; receivers],
            sources: detailed
                .iter()
                .map(|d| {
                    if *d {
                        vec![SourceTotal::default(); sources]
                    } else {
                        vec![]
                    }
                })
                .collect(),
            integration: Default::default(),
        }
    }
    fn merge(
        &mut self,
        key: usize,
        si: usize,
        contributions: Vec<(usize, Contribution)>,
    ) -> Result<(), Diagnostic> {
        for (ri, c) in contributions {
            if ri >= self.energy.len() {
                return Err(Diagnostic::new(
                    "calculation_failed",
                    format!("Work item result names receiver index {ri}, outside the plan"),
                ));
            }
            for b in 0..8 {
                self.energy[ri][b].add(c.energy[b]);
                self.omitted[ri][b].add(c.omitted[b]);
                self.contact[ri][b].add(c.contact_unresolved[b]);
            }
            if let Some(message) = c.contact_message
                && self.contact_messages[ri]
                    .as_ref()
                    .is_none_or(|(first, _)| key < *first)
            {
                self.contact_messages[ri] = Some((key, message));
            }
            self.incomplete[ri] |= c.incomplete;
            if let Some(total) = self.sources[ri].get_mut(si) {
                for b in 0..8 {
                    total.energy[b].add(c.energy[b]);
                    // Missed alternatives are reported as omitted energy.
                    total.omitted[b].add(c.omitted[b]);
                    total.omitted[b].add(c.contact_unresolved[b]);
                }
                total.incomplete |= c.incomplete;
            }
            for (list, values) in [
                (&mut self.warnings[ri], c.warnings),
                (&mut self.ledgers[ri], c.ledgers),
                (&mut self.reports[ri], c.reports),
            ] {
                if !values.is_empty() {
                    list.push((key, values));
                }
            }
        }
        Ok(())
    }
}
fn in_key_order(lists: Vec<Vec<(usize, Vec<Value>)>>) -> Vec<Vec<Value>> {
    lists
        .into_iter()
        .map(|mut list| {
            list.sort_by_key(|(key, _)| *key);
            list.into_iter().flat_map(|(_, values)| values).collect()
        })
        .collect()
}

/// The finished source-receiver transfers of one work item and the work
/// counted while producing them: what a node returns to a coordinator.
#[derive(Serialize, Deserialize)]
pub struct ItemOutput {
    contributions: Vec<(usize, Contribution)>,
    integration: crate::integration::Summary,
    pub stats: PathStats,
}

/// A prepared calculation and its work plan. The plan is built from the
/// request alone, so every machine running the same build prepares the same
/// scene and the same items, and an item index means the same work anywhere.
///
/// Item `i` is (stage, source, receiver block) in stage-major, then source,
/// then block order. Without a pruning strategy there is one stage of every
/// reflection order. Under a strategy the direct paths take two stages, each
/// receiver's leading sources and then its other sources (see `Ranking`),
/// and each reflection order is one further stage.
pub struct Plan {
    request: Value,
    scene: Scene,
    index: SearchIndex,
    /// Receiver blocks of point sources ([0]) and extended sources ([1]).
    partitions: [Vec<ReceiverBlock>; 2],
    /// Each source's first item within a stage; the last entry is the
    /// stage's item count.
    offsets: Vec<usize>,
    stages: Vec<Stage>,
    /// Source ranking at each receiver, under a pruning strategy.
    ranking: Option<Ranking>,
    /// Receivers whose per-source breakdown is accumulated and reported.
    detailed: Vec<bool>,
}
impl Plan {
    /// Validate the request, prepare its scene and divide it into items.
    pub fn prepare(request: Value, cancel: &AtomicBool) -> Result<Self, Diagnostic> {
        Self::with_target(request, cancel, TARGET_ITEMS)
    }
    fn with_target(
        request: Value,
        cancel: &AtomicBool,
        target_items: usize,
    ) -> Result<Self, Diagnostic> {
        crate::validation::validate(&request)?;
        check_cancel(cancel)?;
        if string(&request["settings"], "ground_method", "general") == "mean_path_height"
            && request.get("applicability").is_none()
        {
            let extended = request.get("terrain").is_some()
                || request.get("buildings").is_some()
                || request.get("profile_id").is_some()
                || !array(&request["ground"], "regions").is_empty();
            if extended {
                return Err(Diagnostic::invalid(
                    "Extended mean_path_height inputs require non-pure-tone, predominantly porous, A-weighted applicability declarations",
                ));
            }
        }
        let scene = Scene::prepare(&request, cancel)?;
        let index = SearchIndex::new(&scene);
        // Keep the v1 default for explicit receivers. Generated area points are a
        // spatial field: only their totals are accumulated unless a client
        // explicitly asks for the per-source breakdown.
        let area_details = scene.output["area_source_contributions"] == true;
        let detailed = scene
            .receivers
            .iter()
            .map(|r| r.area.is_none() || area_details)
            .collect();
        // A pruning strategy evaluates the direct paths of each source in two
        // ranked stages and each reflection order as a further stage with live
        // cutoffs (see `Ranking`, `Live`); without a strategy each item covers
        // every order.
        let (stages, ranking) = if scene.settings.strategy.is_some() {
            (
                [Stage::Lead, Stage::Rest]
                    .into_iter()
                    .chain((1..=scene.settings.order).map(Stage::Order))
                    .collect(),
                Some(Ranking::new(&scene, cancel)?),
            )
        } else {
            (vec![Stage::All], None)
        };
        let partitions = [true, false].map(|point| {
            let sources = scene
                .sources
                .iter()
                .filter(|s| (s.kind == "point") == point)
                .count();
            if sources == 0 {
                return vec![];
            }
            let kind = if point { "point" } else { "extended" };
            receiver_blocks(
                sources,
                scene.receivers.len(),
                target_items,
                min_block_receivers(kind),
            )
            .into_iter()
            .map(|range| ReceiverBlock::new(&scene, range))
            .collect()
        });
        let mut offsets = vec![0];
        for source in &scene.sources {
            let blocks = partitions[usize::from(source.kind != "point")].len();
            offsets.push(offsets.last().unwrap() + blocks);
        }
        Ok(Self {
            request,
            scene,
            index,
            partitions,
            offsets,
            stages,
            ranking,
            detailed,
        })
    }
    pub fn sources(&self) -> usize {
        self.scene.sources.len()
    }
    pub fn receivers(&self) -> usize {
        self.scene.receivers.len()
    }
    pub fn items(&self) -> usize {
        self.stages.len() * self.per_stage()
    }
    fn per_stage(&self) -> usize {
        *self.offsets.last().unwrap()
    }
    fn blocks(&self, si: usize) -> &[ReceiverBlock] {
        &self.partitions[usize::from(self.scene.sources[si].kind != "point")]
    }
    /// (stage, source, block) of an item.
    fn locate(&self, item: usize) -> (usize, usize, usize) {
        let (stage, rest) = (item / self.per_stage(), item % self.per_stage());
        let si = self.offsets.partition_point(|first| *first <= rest) - 1;
        (stage, si, rest - self.offsets[si])
    }
    fn trace(
        &self,
        item: usize,
        cutoffs: &ItemCutoffs,
        cancel: &AtomicBool,
        paths: &PathCounter,
    ) -> Result<(Vec<(usize, Contribution)>, crate::integration::Summary), Diagnostic> {
        let (stage, si, bi) = self.locate(item);
        let work = Work {
            cancel,
            operations: &paths.operations,
        };
        trace_item(
            &self.scene,
            &self.index,
            &self.blocks(si)[bi],
            bi == 0,
            &self.scene.sources[si],
            self.stages[stage],
            self.ranking.as_ref(),
            cutoffs,
            &work,
            paths,
        )
    }
    /// Run one item with its own counters, as a node of a distributed
    /// calculation does. `cutoffs` are the coordinator's `Merge::cutoffs`.
    pub fn run_item(
        &self,
        item: usize,
        cutoffs: &ItemCutoffs,
        cancel: &AtomicBool,
    ) -> Result<ItemOutput, Diagnostic> {
        if item >= self.items() {
            return Err(Diagnostic::invalid(format!(
                "Work item {item} is outside the plan's {} items",
                self.items()
            )));
        }
        let paths = PathCounter::new(&|_, _| {});
        let (contributions, integration) = self.trace(item, cutoffs, cancel, &paths)?;
        Ok(ItemOutput {
            contributions,
            integration,
            stats: paths.stats(),
        })
    }
}

/// The merging side of a calculation: receiver totals, live pruning cutoffs,
/// counters and progress. Items may be added in any order, from any machine;
/// sums are exact, so unpruned results do not depend on that order.
pub struct Merge<'a> {
    plan: &'a Plan,
    totals: Mutex<Totals>,
    live: Mutex<Live>,
    ranked: Mutex<Ranked>,
    paths: PathCounter<'a>,
    progress: &'a (dyn Fn(f64, &str) + Sync),
    /// Progress credit of completed items, and the run's total.
    completed: Mutex<f64>,
    total: f64,
    failure: Mutex<Option<Diagnostic>>,
    failed: AtomicBool,
}
impl<'a> Merge<'a> {
    pub fn new(
        plan: &'a Plan,
        progress: &'a (dyn Fn(f64, &str) + Sync),
        path_counts: &'a (dyn Fn(usize, usize) + Sync),
    ) -> Self {
        let scene = &plan.scene;
        Self {
            plan,
            totals: Mutex::new(Totals::new(
                scene.receivers.len(),
                scene.sources.len(),
                &plan.detailed,
            )),
            live: Mutex::new(Live {
                direct_pending: plan.stages.iter().filter(|s| s.direct()).count()
                    * plan.per_stage(),
                used: vec![(0, f64::INFINITY, f64::NEG_INFINITY); scene.settings.order + 1],
            }),
            ranked: Mutex::new(match &plan.ranking {
                Some(ranking) => Ranked {
                    lead: vec![Default::default(); scene.receivers.len()],
                    pending: ranking.leaders.clone(),
                    rest_floor: vec![None; scene.receivers.len()],
                    merged: vec![false; plan.per_stage()],
                    tallies: vec![Tally::default(); plan.items()],
                },
                None => Ranked::default(),
            }),
            paths: PathCounter::new(path_counts),
            progress,
            completed: Mutex::new(0.),
            total: 1. + scene.sources.iter().map(|s| s.weight as f64).sum::<f64>(),
            failure: Mutex::new(None),
            failed: AtomicBool::new(false),
        }
    }
    /// Cutoffs for `item` if it starts now. Under a strategy, a direct item
    /// takes an action for each receiver of its block from the source
    /// ranking, and a reflection item started after every direct item has
    /// completed takes a threshold for each receiver from that receiver's
    /// running total.
    pub fn cutoffs(&self, item: usize) -> ItemCutoffs {
        let plan = self.plan;
        let scene = &plan.scene;
        let (stage, si, bi) = plan.locate(item);
        let range = plan.blocks(si)[bi].range.clone();
        let order = match plan.stages[stage] {
            Stage::All => return ItemCutoffs::fixed(scene.settings.cutoffs),
            Stage::Lead | Stage::Rest => {
                return ItemCutoffs {
                    direct: Some(self.direct(item, plan.stages[stage], si, bi == 0, range)),
                    ..ItemCutoffs::default()
                };
            }
            Stage::Order(order) => order,
        };
        // Locks are taken in the order `add` takes them: totals, then live.
        let totals = self.totals.lock().unwrap_or_else(|e| e.into_inner());
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let thresholds = (live.direct_pending == 0).then(|| {
            let weights = strategy_weights(scene);
            let margin = pruning_margin(
                scene.settings.strategy.as_deref().unwrap_or("dba"),
                scene.sources.len(),
            );
            range
                .map(|ri| {
                    receiver_threshold(
                        &weights,
                        margin,
                        &totals.energy[ri].each_ref().map(Exact::total),
                    )
                })
                .collect::<Vec<_>>()
        });
        drop(totals);
        // Sources are counted once, by their first receiver block.
        let used = &mut live.used[order];
        let mut any = false;
        for t in thresholds.iter().flatten().flatten() {
            used.1 = used.1.min(*t);
            used.2 = used.2.max(*t);
            any = true;
        }
        if !any && bi == 0 {
            used.0 += 1;
        }
        ItemCutoffs {
            thresholds,
            ..ItemCutoffs::default()
        }
    }
    /// The actions of a direct item of source `si` over the receivers
    /// `range`. A lead item evaluates the receivers the source leads at. An
    /// item of the remaining sources evaluates, at each other receiver, the
    /// source unless it is ranked out there; a receiver whose leaders have
    /// not all been merged ranks nothing out yet.
    fn direct(
        &self,
        item: usize,
        stage: Stage,
        si: usize,
        first: bool,
        range: std::ops::Range<usize>,
    ) -> Vec<u8> {
        let scene = &self.plan.scene;
        let ranking = self
            .plan
            .ranking
            .as_ref()
            .expect("a strategy plan has a ranking");
        if stage == Stage::Lead {
            let actions: Vec<u8> = range
                .map(|ri| {
                    if ranking.leads(scene, si, ri) {
                        EVALUATE
                    } else {
                        SKIP
                    }
                })
                .collect();
            let mut ranked = self.ranked.lock().unwrap_or_else(|e| e.into_inner());
            ranked.tallies[item].pairs[0] = actions.iter().filter(|a| **a == EVALUATE).count();
            return actions;
        }
        // Receivers ready to rank, with their floor if already found, or
        // the leaders' energy to find it from.
        let states: Vec<Option<Result<f64, Bands>>> = {
            let ranked = self.ranked.lock().unwrap_or_else(|e| e.into_inner());
            range
                .clone()
                .map(|ri| {
                    (ranked.pending[ri] == 0).then(|| match ranked.rest_floor[ri] {
                        Some(floor) => Ok(floor),
                        None => Err(ranked.lead[ri].each_ref().map(Exact::total)),
                    })
                })
                .collect()
        };
        // Floors are found outside the lock; they depend only on the plan
        // and the leaders' energy, so every caller finds the same value.
        let floors: Vec<Option<f64>> = range
            .clone()
            .zip(states)
            .map(|(ri, state)| {
                state.map(|state| state.unwrap_or_else(|lead| ranking.rest_floor(scene, ri, &lead)))
            })
            .collect();
        let mut ranked = self.ranked.lock().unwrap_or_else(|e| e.into_inner());
        let (mut ranks, mut waits) = (false, false);
        let mut tally = Tally::default();
        let actions = range
            .zip(floors)
            .map(|(ri, floor)| {
                // Kept even where this source leads: the next source's item
                // must not find the floor again.
                if floor.is_some() {
                    ranked.rest_floor[ri] = floor;
                }
                let bound = ranking.weighted(scene, si, ri);
                if bound > 0. && bound >= ranking.lead_floor[ri] {
                    return SKIP;
                }
                let Some(floor) = floor else {
                    tally.pairs[3] += 1;
                    waits = true;
                    return EVALUATE;
                };
                ranks = true;
                // A source without power has nothing to omit: evaluate it.
                if bound > 0. && bound < floor {
                    tally.pairs[2] += 1;
                    OMIT
                } else {
                    tally.pairs[1] += 1;
                    EVALUATE
                }
            })
            .collect();
        tally.unranked = waits && !ranks && first;
        ranked.tallies[item] = tally;
        actions
    }
    fn add(
        &self,
        item: usize,
        contributions: Vec<(usize, Contribution)>,
        integration: crate::integration::Summary,
    ) -> Result<(), Diagnostic> {
        let plan = self.plan;
        let (stage, si, bi) = plan.locate(item);
        if let (Stage::Lead, Some(ranking)) = (plan.stages[stage], &plan.ranking) {
            // The leaders' energy and the receivers they are complete at.
            let mut ranked = self.ranked.lock().unwrap_or_else(|e| e.into_inner());
            let lead = item % plan.per_stage();
            if !std::mem::replace(&mut ranked.merged[lead], true) {
                for (ri, c) in &contributions {
                    if let Some(sum) = ranked.lead.get_mut(*ri) {
                        for (sum, energy) in sum.iter_mut().zip(c.energy) {
                            sum.add(energy);
                        }
                    }
                }
                for ri in plan.blocks(si)[bi].range.clone() {
                    if ranking.leads(&plan.scene, si, ri) {
                        ranked.pending[ri] = ranked.pending[ri].saturating_sub(1);
                    }
                }
            }
        }
        {
            let mut totals = self.totals.lock().unwrap_or_else(|e| e.into_inner());
            totals.merge(stage * plan.sources() + si, si, contributions)?;
            totals.integration.merge(integration);
            if plan.stages[stage].direct() {
                self.live
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .direct_pending -= 1;
            }
        }
        let item_share = 1. / (plan.stages.len() * plan.blocks(si).len()) as f64;
        let mut credit = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        *credit += plan.scene.sources[si].weight as f64 * item_share;
        let amount = 1. + *credit;
        if amount < self.total * (1. - 1e-12) {
            (self.progress)(amount, "propagation")
        }
        Ok(())
    }
    /// Add an item's output returned by `Plan::run_item`.
    pub fn add_output(&self, item: usize, output: ItemOutput) -> Result<(), Diagnostic> {
        if item >= self.plan.items() {
            return Err(Diagnostic::invalid(format!(
                "Work item {item} is outside the plan"
            )));
        }
        self.paths.absorb(&output.stats)?;
        self.add(item, output.contributions, output.integration)
    }
    /// Record a failure; the first one recorded is the calculation's error.
    pub fn fail(&self, error: Diagnostic) {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(error);
        self.failed.store(true, Ordering::Relaxed);
    }
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
    /// Assemble the result once every item has been added.
    pub fn finish(self, cancel: &AtomicBool) -> Result<Value, Diagnostic> {
        let Merge {
            plan,
            totals,
            live,
            ranked,
            paths,
            failure,
            ..
        } = self;
        let Plan {
            request,
            scene,
            detailed,
            ..
        } = plan;
        let failure = failure.into_inner().unwrap_or_else(|e| e.into_inner());
        let ranked = ranked.into_inner().unwrap_or_else(|e| e.into_inner());
        // One report per reflection order. Direct paths have no weighted
        // cutoff: they are ranked (see `diagnostics.pruning.direct`).
        let stage_reports: Vec<Value> = live
            .into_inner()
            .unwrap_or_else(|e| e.into_inner())
            .used
            .iter()
            .enumerate()
            .map(|(order, (unpruned, lo, hi))| {
                let unpruned = if order == 0 {
                    ranked.unranked()
                } else {
                    *unpruned
                };
                json!({"reflection_order":order,"tasks_without_cutoff":unpruned,
                    "weighted_cutoff_db":if lo.is_finite() {json!([lo, hi])} else {Value::Null}})
            })
            .collect();
        let Totals {
            energy,
            omitted,
            contact,
            contact_messages,
            warnings,
            ledgers: receiver_ledgers,
            reports: receiver_reports,
            incomplete,
            mut sources,
            integration,
            ..
        } = totals.into_inner().unwrap_or_else(|e| e.into_inner());
        let contact_messages: Vec<Option<String>> = contact_messages
            .into_iter()
            .map(|m| m.map(|(_, message)| message))
            .collect();
        let mut receiver_warnings = in_key_order(warnings);
        let mut receiver_ledgers = in_key_order(receiver_ledgers);
        let mut receiver_reports = in_key_order(receiver_reports);
        let counts = paths.flush();
        check_cancel(cancel)?;
        if let Some(error) = failure {
            return Err(error);
        }
        let mut receiver_results = vec![];
        let mut ledgers = vec![];
        let mut reports = vec![];
        for (ri, r) in scene.receivers.iter().enumerate() {
            check_cancel(cancel)?;
            let computed = energy[ri].each_ref().map(Exact::total);
            let cutoff = omitted[ri].each_ref().map(Exact::total);
            let allowance = contact[ri].each_ref().map(Exact::total);
            if let Some(message) = &contact_messages[ri] {
                // Incomplete joint searches remove at most `allowance` of capped
                // point-family energy. Accept that only within the receiver-total
                // tolerance; the bound is always reported as omitted energy.
                let fits = (0..8).all(|b| {
                    allowance[b].is_finite()
                        && allowance[b]
                            <= scene.settings.absolute
                                + scene.settings.relative * (computed[b] + cutoff[b])
                });
                if !fits {
                    // Bounded and already in omitted energy, but above tolerance.
                    receiver_warnings[ri].push(
                        json!({"kind":"unresolved_pathways_above_tolerance",
                        "receiver_id":r.id,"bounded":true,"allowance":allowance,
                        "receiver_energy":computed,"receiver_omitted_energy":cutoff,
                        "relative_tolerance":scene.settings.relative,"message":message}),
                    );
                }
                receiver_reports[ri].push(json!({"receiver_id":r.id,
                    "boundary_tolerance_scope":"receiver_total","unresolved_contact_allowance":allowance,
                    "receiver_energy":computed,"receiver_omitted_energy":cutoff,
                    "absolute_tolerance":scene.settings.absolute,"relative_tolerance":scene.settings.relative,
                    "first_unresolved_contact":message}));
            }
            let reported: Bands = std::array::from_fn(|b| cutoff[b] + allowance[b]);
            let mut result = fields(computed, reported)?;
            result["receiver_id"] = json!(r.id);
            result["position"] = json!(r.point);
            result["ground_elevation_m"] = json!(r.ground);
            result["height_m"] = json!(r.point[2] - r.ground);
            if detailed[ri] {
                let list: Result<Vec<Value>, Diagnostic> = std::mem::take(&mut sources[ri])
                    .into_iter()
                    .zip(&scene.sources)
                    .map(|(t, source)| {
                        // Unconnected explicit receivers report the source as exact zero.
                        let mut out = fields(
                            t.energy.each_ref().map(Exact::total),
                            t.omitted.each_ref().map(Exact::total),
                        )?;
                        if t.incomplete {
                            out["complete"] = json!(false);
                        }
                        out["source_id"] = json!(source.id);
                        Ok(out)
                    })
                    .collect();
                result["sources"] = json!(list?);
            }
            if !receiver_warnings[ri].is_empty() {
                result["warnings"] = json!(std::mem::take(&mut receiver_warnings[ri]));
            }
            if incomplete[ri] {
                result["complete"] = json!(false);
            }
            if let Some(a) = &r.area {
                result["area_receiver_id"] = json!(a);
                result["grid_column"] = json!(r.column);
                result["grid_row"] = json!(r.row)
            }
            ledgers.append(&mut receiver_ledgers[ri]);
            reports.append(&mut receiver_reports[ri]);
            receiver_results.push(result)
        }
        check_cancel(cancel)?;
        let mut ground = request.get("ground").cloned().unwrap_or(json!({}));
        ground["factor"] = json!(scene.ground_factor);
        ground["elevation_m"] = json!(number(&request["ground"], "elevation_m", 0.));
        ground["regions"] = json!(array(&request["ground"], "regions"));
        ground["region_policy"] = json!(scene.ground_policy);
        let caps = crate::capabilities();
        // Run-level count of pathway problems that did not stop the calculation.
        let mut warning_summary = json!({"warnings":0,"receivers_with_warnings":0,
            "incomplete_receivers":0,"failed_pathways":0,"bounded_above_tolerance":0});
        for receiver in &receiver_results {
            let Some(list) = receiver.get("warnings").and_then(Value::as_array) else {
                continue;
            };
            let count = |kind: &str| list.iter().filter(|w| w["kind"] == kind).count() as u64;
            let add = |summary: &mut Value, key: &str, n: u64| {
                summary[key] = json!(summary[key].as_u64().unwrap_or(0) + n);
            };
            add(&mut warning_summary, "warnings", list.len() as u64);
            add(&mut warning_summary, "receivers_with_warnings", 1);
            add(
                &mut warning_summary,
                "failed_pathways",
                count("failed_pathway"),
            );
            add(
                &mut warning_summary,
                "bounded_above_tolerance",
                count("unresolved_pathways_above_tolerance")
                    + count("integration_tolerance_exceeded"),
            );
            if receiver.get("complete") == Some(&json!(false)) {
                add(&mut warning_summary, "incomplete_receivers", 1);
            }
        }
        let mut result = json!({"api_version":"1.2","implementation":caps["implementation"],"algorithm_revision":caps["algorithm_revision"],"method":"ISO 9613-2:2024 with ISO 9613-1:1993 atmospheric absorption","profile":"ISO 9613-2:2024 Rust in-memory propagation profile; see limitations","profile_id":"iso_rust_2024","component_profiles":crate::capabilities::component_profiles_for(request),"atmosphere":scene.atmosphere,"ground":ground,"settings":scene.settings.json(),"applicability":request.get("applicability").cloned().unwrap_or_else(||if scene.settings.ground_method=="mean_path_height"{json!({"sound_character":"not_pure_tone","ground_character":"porous_or_predominantly_porous","assessment_quantity":"a_weighted","evidence":"Legacy explicit ground-method selection"})}else{json!({"sound_character":"unknown","ground_character":"unknown","assessment_quantity":"octave"})}),"receivers":receiver_results,"area_receivers":scene.area_summaries,"limitations":caps["limitations"],"diagnostics":{"path_counts":counts,"ground_method":scene.settings.ground_method,"octave_ground_approximation":scene.settings.ground_method=="mean_path_height","assessment_quantity":if scene.settings.ground_method=="mean_path_height"{"a_weighted"}else{"octave"},"integration_converged":true,"omission_basis":"Each omission is a conservative upper bound on a family skipped before path construction; computed contributions are never discarded. Families without such a bound are calculated.","unbounded_prefix_cuts":0,"integration":reports},"operations":paths.operations.load(Ordering::Relaxed)});
        result["warning_summary"] = warning_summary;
        result["diagnostics"]["integration_summary"] = integration.json(
            scene,
            string(&scene.output, "integration_reports", "summary"),
        );
        if request.pointer("/output/source_power_transfers") == Some(&Value::Bool(true)) {
            // Contributions remain reference SPLs in the ordinary result shape.
            // Consumers subtract reference_db and must reject omissions/incomplete
            // pathways before reusing the transfer at other source powers.
            result["source_power_transfers"] = json!({
                "reference_db":100., "power_basis":"per_source_input", "pruning":"disabled"
            });
        }
        if let Some(strategy) = &scene.settings.strategy {
            let pairs = ranked.pairs();
            result["diagnostics"]["pruning"] = json!({"strategy":strategy,
                "margin_db":pruning_margin(strategy, scene.sources.len()),"source_count":scene.sources.len(),
                "rule":format!("direct paths are ranked at each receiver by an upper bound: sources within {c} dB of the largest bound are evaluated first, then the lowest-bound others are omitted while their bounds sum, in every band, to at most 10^(-{c}/10) of the energy those leaders delivered there in that band; once all direct paths have completed, each reflection task takes, for each receiver, band cutoff = that receiver's weighted level from contributions completed when the task starts - margin - band weighting; a receiver with no energy yet is not pruned; reflection prefixes use the lowest threshold of the task's receiver block; margin = 10 log10(source count) + {c} dB", c = pruning_constant(strategy)),
                "direct":{"leading_pairs":pairs[0],"evaluated_pairs":pairs[1],
                    "omitted_pairs":pairs[2],"unranked_pairs":pairs[3]},
                "stages":stage_reports});
        }
        result["diagnostics"]["path_search"] = json!({
            "counting_unit": "source path stub / reflection beam; independent of receivers and octaves",
            "receiver_candidates": paths.receiver_candidates.load(Ordering::Relaxed),
            "receiver_connections": paths.receiver_connections.load(Ordering::Relaxed),
            "receiver_stub_counts": {
                "checked": paths.receiver_connections.load(Ordering::Relaxed),
                "pruned": paths.receiver_stubs_pruned.load(Ordering::Relaxed),
            },
            "reflector_candidates": paths.reflector_candidates.load(Ordering::Relaxed),
            "prefix_pruned_beams": paths.prefix_pruned.load(Ordering::Relaxed),
            "empty_beams": paths.empty_beams.load(Ordering::Relaxed),
            "geometry_rejected": paths.geometry_rejected.load(Ordering::Relaxed),
            "bounded_pruned_bands": paths.bounded_bands.load(Ordering::Relaxed),
        });
        if scene.output["prepared_geometry"] == true {
            result["prepared_geometry"] = scene.prepared_json()
        }
        if string(&scene.output, "path_diagnostics", "none") != "none" {
            result["path_diagnostics"] = json!(ledgers)
        }
        Ok(result)
    }
}

pub fn calculate(
    request: &Value,
    cancel: &AtomicBool,
    progress: &(dyn Fn(f64, &str) + Sync),
) -> Result<Value, Diagnostic> {
    calculate_with_summary(request, cancel, progress, &|_, _| {}, &|_, _| {})
}

pub fn calculate_with_summary(
    request: &Value,
    cancel: &AtomicBool,
    progress: &(dyn Fn(f64, &str) + Sync),
    summary: &(dyn Fn(usize, usize) + Sync),
    path_counts: &(dyn Fn(usize, usize) + Sync),
) -> Result<Value, Diagnostic> {
    calculate_planned(
        request,
        cancel,
        progress,
        summary,
        path_counts,
        TARGET_ITEMS,
    )
}

/// `calculate_with_summary` with an explicit work-plan size, so tests can
/// check that results do not depend on how the work is divided.
fn calculate_planned(
    request: &Value,
    cancel: &AtomicBool,
    progress: &(dyn Fn(f64, &str) + Sync),
    summary: &(dyn Fn(usize, usize) + Sync),
    path_counts: &(dyn Fn(usize, usize) + Sync),
    target_items: usize,
) -> Result<Value, Diagnostic> {
    let plan = Plan::with_target(request.clone(), cancel, target_items)?;
    summary(plan.sources(), plan.receivers());
    progress(1., "propagation");
    let merge = Merge::new(&plan, progress, path_counts);
    // Items start in order-major sequence whenever a worker is free; no
    // order waits for the previous one. Items share the merge's counters.
    // Under a strategy the lead items finish first, so that every receiver
    // is ranked from all of its leaders and the direct result is the same in
    // every run; they are direct paths only, so the wait is short.
    let run = |item: usize| {
        if merge.failed() {
            return;
        }
        let cutoffs = merge.cutoffs(item);
        if let Err(error) = plan
            .trace(item, &cutoffs, cancel, &merge.paths)
            .and_then(|(out, integration)| merge.add(item, out, integration))
        {
            merge.fail(error);
        }
    };
    let in_flight = 2 * rayon::current_num_threads().max(1);
    let lead = if plan.ranking.is_some() {
        plan.per_stage()
    } else {
        0
    };
    bounded(lead, in_flight, run);
    bounded(plan.items() - lead, in_flight, |item| run(item + lead));
    merge.finish(cancel)
}

#[cfg(test)]
mod incomplete_contact_tests {
    use serde_json::{Value, json};
    use std::sync::atomic::AtomicBool;

    // Constructed scene, not an ISO case. Source s at (0,0,1), receiver at
    // (60,0,1). A barrier at x=25 (y <= 19) screens the direct path and the
    // first leg of the reflection at (30,20) from the y=20 reflector, whose
    // joint search is forced incomplete by the test-only trigger. The top
    // route is screened, so up to cap - found energy may be missing.
    fn request(near_source: bool) -> Value {
        with_reflector(near_source, "test_incomplete_contact_search")
    }

    fn with_reflector(near_source: bool, reflector: &str) -> Value {
        let mut sources = vec![
            json!({"id":"s","kind":"point","position":[0.,0.,1.],"spectrum":vec![90.0_f64; 8]}),
        ];
        if near_source {
            sources.push(json!({"id":"near","kind":"point","position":[59.,0.,1.],"spectrum":vec![90.0_f64; 8]}));
        }
        json!({
            "sources":sources,
            "receivers":[{"id":"r","position":[60.,0.,1.]}],
            "barriers":[{"id":"screen","start":[25.,-100.,0.],"end":[25.,19.,0.],"height_m":6,"closed":true,"surface_density_kg_m2":10}],
            "reflectors":[{"id":reflector,"vertices":
                [[-100.,20.,0.],[200.,20.,0.],[200.,20.,30.],[-100.,20.,30.]]}],
            "settings":{"max_reflect_order":1,"minimum_contribution_db":null}
        })
    }

    fn warnings(result: &Value) -> Vec<Value> {
        result["receivers"][0]["warnings"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn missed_alternatives_beyond_the_receiver_tolerance_complete_with_a_warning() {
        let result =
            crate::calculate(&request(false), &AtomicBool::new(false), &|_, _| {}).unwrap();
        let receiver = &result["receivers"][0];
        let list = warnings(&result);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["kind"], "unresolved_pathways_above_tolerance");
        assert_eq!(list[0]["bounded"], true);
        assert!(
            list[0]["message"]
                .as_str()
                .unwrap()
                .contains("Test-only incomplete joint search")
        );
        // Bounded: the receiver stays complete and the bound is omitted energy.
        assert!(receiver.get("complete").is_none());
        for b in 0..8 {
            assert!(
                receiver["omitted_energy"][b].as_f64().unwrap()
                    >= list[0]["allowance"][b].as_f64().unwrap()
            );
        }
        assert_eq!(result["warning_summary"]["bounded_above_tolerance"], 1);
        assert_eq!(result["warning_summary"]["incomplete_receivers"], 0);
    }

    #[test]
    fn a_failed_pathway_completes_the_run_and_marks_the_receiver_incomplete() {
        let result = crate::calculate(
            &with_reflector(true, "test_failed_pathway"),
            &AtomicBool::new(false),
            &|_, _| {},
        )
        .unwrap();
        let receiver = &result["receivers"][0];
        assert_eq!(receiver["complete"], false);
        assert!(receiver["a_weighted_db"].is_number());
        let list = warnings(&result);
        assert!(!list.is_empty());
        assert!(
            list.iter()
                .all(|w| w["kind"] == "failed_pathway" && w["bounded"] == false)
        );
        assert!(list.iter().any(|w| w["source_id"] == "s"
            && w["reflection_faces"] == json!(["test_failed_pathway/face"])));
        let failed = receiver["sources"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["complete"] == false)
            .count();
        assert!(failed > 0);
        assert_eq!(result["warning_summary"]["incomplete_receivers"], 1);
        assert_eq!(
            result["warning_summary"]["failed_pathways"],
            list.len() as u64
        );
    }

    #[test]
    fn a_failed_reference_is_a_pathway_warning_not_a_job_failure() {
        let result = crate::calculate(
            &with_reflector(true, "test_failed_reference"),
            &AtomicBool::new(false),
            &|_, _| {},
        )
        .unwrap();
        let receiver = &result["receivers"][0];
        assert_eq!(receiver["complete"], false);
        assert!(receiver["a_weighted_db"].is_number());
        let list = warnings(&result);
        assert!(!list.is_empty());
        assert!(list.iter().all(|w| {
            w["kind"] == "failed_pathway"
                && w["bounded"] == false
                && w["code"] == "nonfinite_result"
                && w["reflection_faces"] == json!(["test_failed_reference/face"])
                && w["message"]
                    .as_str()
                    .unwrap()
                    .contains("Test-only failed reference")
        }));
        assert_eq!(result["warning_summary"]["incomplete_receivers"], 1);
    }

    /// Two reflectors meeting at an inside corner, with the receiver placed so
    /// the second-order contacts lie micrometres apart. The image-method
    /// reference is specular by construction; rounding in projected
    /// coordinates (here 7e5 m) must not reject it, and the result must match
    /// the same scene at the origin.
    fn corner_request(offset: [f64; 2], delta: f64) -> Value {
        let at = |x: f64, y: f64, z: f64| json!([x + offset[0], y + offset[1], z]);
        json!({
            "sources":[{"id":"s","kind":"point","position":at(3.,4.,2.),"spectrum":vec![90.0_f64; 8]}],
            "receivers":[{"id":"r","position":at(6.,8.+delta,2.)}],
            "reflectors":[
                {"id":"A","start":at(0.,-5.,0.),"end":at(0.,10.,0.),"height_m":10},
                {"id":"B","start":at(-5.,0.,0.),"end":at(10.,0.,0.),"height_m":10}
            ],
            "ground":{"factor":0.0},
            "settings":{"max_reflect_order":2,"minimum_contribution_db":null}
        })
    }

    #[test]
    fn corner_reflection_contacts_micrometres_apart_survive_projected_coordinates() {
        for delta in [1e-6, 1e-7] {
            let level = |offset| {
                let result = crate::calculate(
                    &corner_request(offset, delta),
                    &AtomicBool::new(false),
                    &|_, _| {},
                )
                .unwrap();
                assert!(warnings(&result).is_empty());
                result["receivers"][0]["a_weighted_db"].as_f64().unwrap()
            };
            let origin = level([0., 0.]);
            let projected = level([713_800., 345_271.]);
            assert!(
                (origin - projected).abs() < 1e-6,
                "delta {delta}: {origin} vs {projected}"
            );
        }
    }

    #[test]
    fn unsupported_pathways_still_stop_the_run() {
        let error = crate::calculate(
            &with_reflector(false, "test_unsupported_pathway"),
            &AtomicBool::new(false),
            &|_, _| {},
        )
        .unwrap_err();
        assert_eq!(error.code, "unsupported");
    }

    #[test]
    fn negligible_missed_alternatives_are_reported_as_omitted_energy() {
        let result = crate::calculate(&request(true), &AtomicBool::new(false), &|_, _| {}).unwrap();
        let reports: Vec<_> = result["diagnostics"]["integration"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r.get("unresolved_contact_allowance").is_some())
            .collect();
        assert_eq!(reports.len(), 1);
        let allowance: Vec<f64> = reports[0]["unresolved_contact_allowance"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        assert!(allowance.iter().all(|a| *a > 0.), "{allowance:?}");
        let receiver = &result["receivers"][0];
        for b in 0..8 {
            let omitted = receiver["omitted_energy"][b].as_f64().unwrap();
            let energy = reports[0]["receiver_energy"][b].as_f64().unwrap();
            assert!(omitted >= allowance[b], "band {b}");
            assert!(allowance[b] <= 1e-3 * energy, "band {b}");
        }
        // The screened source carries the bound in its own omitted energy.
        let screened = receiver["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["source_id"] == "s")
            .unwrap();
        for b in 0..8 {
            assert!(screened["omitted_energy"][b].as_f64().unwrap() >= allowance[b]);
        }
    }
}

#[cfg(test)]
mod work_plan_tests {
    use super::{calculate_planned, receiver_blocks};
    use serde_json::{Value, json};
    use std::sync::atomic::AtomicBool;

    /// Point, line and area sources, two buildings (second-order reflections),
    /// explicit receivers and a generated area grid.
    fn request() -> Value {
        json!({
            "sources":[
                {"id":"p","kind":"point","position":[-12,-6,2],"spectrum":[80,82,84,86,88,86,84,82]},
                {"id":"l","kind":"line","vertices":[[-20,30,1],[-17,31,1]],"power_basis":"power_per_m","spectrum":[70,72,74,76,78,76,74,72]},
                {"id":"a","kind":"area","triangles":[[[25,-12,1],[27,-12,1],[25,-10,1]]],"power_basis":"total_power","spectrum":[85,85,85,85,85,85,85,85]}
            ],
            "receivers":[{"id":"r1","position":[-10,15,2]},{"id":"r2","position":[22,18,4]},{"id":"r3","position":[5,-15,1.5]}],
            "area_receivers":[{"id":"grid","boundary":[[-30,-30],[40,-30],[40,45],[-30,45]],"spacing_m":15,"height_m":1.5}],
            "buildings":[
                {"id":"b1","footprint":[[0,0,0],[10,0,0],[10,10,0],[0,10,0]],"height_m":10},
                {"id":"b2","footprint":[[15,5,0],[20,5,0],[20,25,0],[15,25,0]],"height_m":8}
            ],
            "ground":{"factor":0.5},
            "settings":{"max_reflect_order":2,"minimum_contribution_db":null,"area_relative_tolerance":0.05},
            "output":{"area_source_contributions":true}
        })
    }
    fn run_local(request: &Value, threads: usize) -> Value {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| crate::calculate(request, &AtomicBool::new(false), &|_, _| {}).unwrap())
    }
    fn run(request: &Value, threads: usize, target_items: usize) -> Value {
        let mut value = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| {
                calculate_planned(
                    request,
                    &AtomicBool::new(false),
                    &|_, _| {},
                    &|_, _| {},
                    &|_, _| {},
                    target_items,
                )
                .unwrap()
            });
        // Operation counts include tracing repeated once per receiver block.
        value["operations"] = Value::Null;
        value
    }

    #[test]
    fn blocks_cover_every_receiver_once_and_grow_only_for_few_sources() {
        for (sources, receivers, target) in [
            (3, 151, 1024),
            (2000, 151, 1024),
            (1, 0, 1024),
            (5, 20000, 7),
            (2, 100_000, 1024),
        ] {
            let blocks = receiver_blocks(sources, receivers, target, 16);
            assert_eq!(blocks.first().unwrap().start, 0);
            assert_eq!(blocks.last().unwrap().end, receivers);
            assert!(blocks.windows(2).all(|w| w[0].end == w[1].start));
            assert!(blocks.iter().all(|b| b.len() <= super::MAX_BLOCK_RECEIVERS));
        }
        assert_eq!(receiver_blocks(2000, 151, 1024, 16).len(), 1);
        assert_eq!(receiver_blocks(3, 151, 1024, 16).len(), 10);
        assert_eq!(receiver_blocks(3, 151, 1024, 1).len(), 151);
    }

    /// Point sources keep 16-receiver blocks; line and area sources are
    /// divided down to single receivers, and items still cover every
    /// source-receiver pair once.
    #[test]
    fn extended_sources_get_finer_blocks_than_point_sources() {
        let request = request();
        let plan = super::Plan::prepare(request, &AtomicBool::new(false)).unwrap();
        let receivers = plan.receivers();
        assert!(receivers > 16);
        let [point, extended] = &plan.partitions;
        assert_eq!(point.len(), receivers.div_ceil(16));
        assert_eq!(extended.len(), receivers);
        assert_eq!(plan.items(), point.len() + 2 * extended.len());
        let mut covered = vec![0; 3 * receivers];
        for item in 0..plan.items() {
            let (_, si, bi) = plan.locate(item);
            for ri in plan.blocks(si)[bi].range.clone() {
                covered[si * receivers + ri] += 1;
            }
        }
        assert!(covered.iter().all(|c| *c == 1));
    }

    /// Items run in isolation as a node runs them, carried as JSON and merged
    /// in another order, give the local result bit for bit, diagnostics and
    /// operation counts included.
    #[test]
    fn isolated_items_merged_in_any_order_match_the_local_run() {
        use super::{ItemOutput, Merge, Plan};
        let mut request = request();
        request["output"]["path_diagnostics"] = json!("summary");
        request["sources"] = json!([request["sources"][0], request["sources"][1]]);
        let local = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap()
            .install(|| crate::calculate(&request, &AtomicBool::new(false), &|_, _| {}).unwrap());
        let cancel = AtomicBool::new(false);
        let plan = Plan::prepare(request.clone(), &cancel).unwrap();
        assert!(plan.items() > 2);
        let merge = Merge::new(&plan, &|_, _| {}, &|_, _| {});
        let mut items: Vec<usize> = (0..plan.items()).collect();
        items.reverse();
        items.rotate_left(plan.items() / 3);
        for item in items {
            let cutoffs = merge.cutoffs(item);
            let wire =
                serde_json::to_vec(&plan.run_item(item, &cutoffs, &cancel).unwrap()).unwrap();
            let output: ItemOutput = serde_json::from_slice(&wire).unwrap();
            merge.add_output(item, output).unwrap();
        }
        let distributed = merge.finish(&cancel).unwrap();
        assert!(distributed["path_diagnostics"].as_array().unwrap().len() > 20);
        assert_eq!(
            serde_json::to_string(&distributed).unwrap(),
            serde_json::to_string(&local).unwrap()
        );
    }

    /// The cutoffs of an item follow the receivers of its block: a fixed
    /// cutoff is the same everywhere, a strategy gives each receiver its own,
    /// and work shared by the block uses none higher than any receiver's.
    #[test]
    fn item_cutoffs_follow_the_block_receivers() {
        use super::{ItemCutoffs, receiver_threshold};
        let weights = crate::acoustics::A_WEIGHTING;
        let fixed = ItemCutoffs::fixed([Some(10.); 8]);
        assert_eq!(fixed.at(&weights, 3), [Some(10.); 8]);
        assert_eq!(fixed.floor(&weights), [Some(10.); 8]);
        let unpruned = ItemCutoffs::default();
        assert_eq!(unpruned.at(&weights, 0), [None; 8]);
        assert_eq!(unpruned.floor(&weights), [None; 8]);
        let live = ItemCutoffs {
            thresholds: Some(vec![Some(40.), None, Some(25.)]),
            ..ItemCutoffs::default()
        };
        for b in 0..8 {
            assert_eq!(live.at(&weights, 0)[b], Some(40. - weights[b]));
            assert_eq!(live.at(&weights, 2)[b], Some(25. - weights[b]));
        }
        // A receiver without energy, or outside the list, is not pruned, and
        // nothing shared by the block is cut on its behalf.
        assert_eq!(live.at(&weights, 1), [None; 8]);
        assert_eq!(live.at(&weights, 3), [None; 8]);
        assert_eq!(live.floor(&weights), [None; 8]);
        let all = ItemCutoffs {
            thresholds: Some(vec![Some(40.), Some(25.), Some(31.)]),
            ..ItemCutoffs::default()
        };
        for b in 0..8 {
            assert_eq!(all.floor(&weights)[b], Some(25. - weights[b]));
        }
        let empty = ItemCutoffs {
            thresholds: Some(vec![]),
            ..ItemCutoffs::default()
        };
        assert_eq!(empty.floor(&weights), [None; 8]);
        // Threshold = weighted level less the margin; none without energy.
        assert_eq!(receiver_threshold(&weights, 20., &[0.; 8]), None);
        let mut energy = [0.; 8];
        energy[4] = 1e6;
        let t = receiver_threshold(&weights, 20., &energy).unwrap();
        assert!((t - 40.).abs() < 1e-12, "{t}");
    }

    /// With a strategy, items run one after another as a node runs them,
    /// carried as JSON, give the one-worker local result bit for bit: every
    /// item then starts from the same completed contributions in both runs.
    /// (Runs with several workers differ, because thresholds are live.)
    #[test]
    fn serial_strategy_items_match_the_one_worker_run() {
        use super::{ItemOutput, Merge, Plan};
        let mut request = request();
        request["sources"] = json!([request["sources"][0], request["sources"][1]]);
        request["settings"]["pruning_strategy"] = json!("dba");
        let local = run_local(&request, 1);
        let cancel = AtomicBool::new(false);
        let plan = Plan::prepare(request.clone(), &cancel).unwrap();
        let merge = Merge::new(&plan, &|_, _| {}, &|_, _| {});
        let mut applied = 0;
        for item in 0..plan.items() {
            let cutoffs = merge.cutoffs(item);
            if let Some(thresholds) = &cutoffs.thresholds {
                applied += thresholds.iter().flatten().count();
            }
            let wire =
                serde_json::to_vec(&plan.run_item(item, &cutoffs, &cancel).unwrap()).unwrap();
            let output: ItemOutput = serde_json::from_slice(&wire).unwrap();
            merge.add_output(item, output).unwrap();
        }
        assert!(applied > 0, "no receiver threshold was applied");
        let serial = merge.finish(&cancel).unwrap();
        assert_eq!(
            serde_json::to_string(&serial).unwrap(),
            serde_json::to_string(&local).unwrap()
        );
    }

    /// A threshold list that does not fit its item is an invalid input,
    /// never applied: the wrong length (it would reach the wrong receivers),
    /// a value that is not finite, a list beside fixed cutoffs, and a list
    /// for a direct item or for a plan without a strategy.
    #[test]
    fn a_threshold_list_that_does_not_fit_its_item_is_rejected() {
        use super::{ItemCutoffs, Plan};
        let mut request = request();
        request["sources"] = json!([request["sources"][0]]);
        let cancel = AtomicBool::new(false);
        let unpruned = Plan::prepare(request.clone(), &cancel).unwrap();
        request["settings"]["pruning_strategy"] = json!("dba");
        let plan = Plan::prepare(request, &cancel).unwrap();
        let item = plan.items() - 1;
        let (stage, si, bi) = plan.locate(item);
        assert_eq!(plan.stages[stage], super::Stage::Order(2));
        let receivers = plan.blocks(si)[bi].range.len();
        let list = |thresholds: Vec<Option<f64>>| ItemCutoffs {
            thresholds: Some(thresholds),
            ..ItemCutoffs::default()
        };
        let rejected = |plan: &Plan, item: usize, cutoffs: &ItemCutoffs| {
            let error = plan.run_item(item, cutoffs, &cancel).err().unwrap();
            assert_eq!(error.code, "invalid_input", "{error:?}");
        };
        let right = list(vec![Some(30.); receivers]);
        assert!(plan.run_item(item, &right, &cancel).is_ok());
        assert!(
            plan.run_item(item, &list(vec![None; receivers]), &cancel)
                .is_ok()
        );
        for wrong in [receivers + 1, receivers - 1] {
            rejected(&plan, item, &list(vec![Some(30.); wrong]));
        }
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut thresholds = vec![Some(30.); receivers];
            thresholds[receivers - 1] = Some(bad);
            rejected(&plan, item, &list(thresholds));
        }
        let mut both = right.clone();
        both.fixed = [Some(10.); 8];
        rejected(&plan, item, &both);
        // Direct items (the first two stages) and plans without a strategy
        // take no thresholds.
        let direct_block = plan.blocks(0)[0].range.len();
        rejected(&plan, 0, &list(vec![Some(30.); direct_block]));
        rejected(&unpruned, 0, &list(vec![Some(30.); direct_block]));
        // Without a list, band cutoffs apply to every receiver, as before.
        assert!(
            plan.run_item(item, &ItemCutoffs::fixed([Some(10.); 8]), &cancel)
                .is_ok()
        );
        assert!(
            plan.run_item(item, &ItemCutoffs::default(), &cancel)
                .is_ok()
        );
        // A direct item takes one action per receiver and nothing else: a
        // lead item (0) evaluates or skips, an item of the remaining
        // sources (the next stage) may also omit.
        use super::{EVALUATE, OMIT, SKIP};
        let actions = |actions: Vec<u8>| ItemCutoffs {
            direct: Some(actions),
            ..ItemCutoffs::default()
        };
        let rest = plan.per_stage();
        assert_eq!(plan.stages[plan.locate(rest).0], super::Stage::Rest);
        let mixed: Vec<u8> = (0..direct_block).map(|k| [SKIP, EVALUATE][k % 2]).collect();
        assert!(plan.run_item(0, &actions(mixed.clone()), &cancel).is_ok());
        assert!(
            plan.run_item(rest, &actions(vec![OMIT; direct_block]), &cancel)
                .is_ok()
        );
        // Without the list both direct stages would evaluate every receiver.
        rejected(&plan, 0, &ItemCutoffs::default());
        rejected(&plan, rest, &ItemCutoffs::default());
        rejected(&plan, 0, &ItemCutoffs::fixed([Some(10.); 8]));
        rejected(&plan, 0, &actions(vec![EVALUATE; direct_block + 1]));
        rejected(&plan, 0, &actions(vec![OMIT; direct_block]));
        rejected(&plan, rest, &actions(vec![3; direct_block]));
        rejected(&plan, item, &actions(vec![EVALUATE; receivers]));
        rejected(&unpruned, 0, &actions(vec![EVALUATE; direct_block]));
        let mut both = actions(mixed);
        both.thresholds = Some(vec![Some(30.); direct_block]);
        rejected(&plan, 0, &both);
    }

    /// Receivers in a later block of a source are pruned against their own
    /// levels. Loud and quiet receivers alternate, so every block holds both
    /// and a threshold applied to the wrong receiver, or to none, shows.
    #[test]
    fn later_blocks_apply_each_receivers_own_threshold() {
        use super::Plan;
        let mut buildings = vec![];
        for (i, (x, y)) in [(0., 0.), (60., 0.), (0., 60.), (60., 60.), (30., 120.)]
            .iter()
            .enumerate()
        {
            buildings.push(json!({"id":format!("b{i}"),"footprint":[[x,y,0],[x+20.,*y,0],[x+20.,y+20.,0],[*x,y+20.,0]],"height_m":12}));
        }
        let sources: Vec<Value> = (0..12)
            .map(|i| {
                json!({"id":format!("s{i}"),"kind":"point","position":[5. + 7. * i as f64, 40. + (i % 3) as f64 * 5., 2],
                "spectrum":[85,88,90,92,95,92,88,84]})
            })
            .collect();
        // 48 receivers: loud (in the courtyard), middle and quiet (far away)
        // in turn, so each of the three 16-receiver blocks mixes all three.
        let receivers: Vec<Value> = (0..48)
            .map(|i| {
                let k = (i / 3) as f64;
                let position = match i % 3 {
                    0 => [25. + 2. * k, 30., 1.5],
                    1 => [150. + 2. * k, 45., 1.5],
                    _ => [400. + 2. * k, -200., 1.5],
                };
                json!({"id":format!("r{i}"),"position":position})
            })
            .collect();
        let mut request = json!({"sources":sources,"receivers":receivers,"buildings":buildings,
            "settings":{"max_reflect_order":2,"minimum_contribution_db":null}});
        let weights = crate::acoustics::A_WEIGHTING;
        let weighted = |r: &Value| {
            let e: f64 = (0..8)
                .map(|b| {
                    r["octave_db"][b]
                        .as_f64()
                        .map_or(0., |l| 10f64.powf((l + weights[b]) / 10.))
                })
                .sum();
            10. * e.log10()
        };
        let uncut: Vec<f64> = run_local(&request, 4)["receivers"]
            .as_array()
            .unwrap()
            .iter()
            .map(weighted)
            .collect();
        let quietest = uncut.iter().copied().fold(f64::INFINITY, f64::min);
        request["settings"]["pruning_strategy"] = json!("dba");
        request["output"] = json!({"path_diagnostics":"summary"});
        let cancel = AtomicBool::new(false);
        let plan = Plan::prepare(request.clone(), &cancel).unwrap();
        let blocks: Vec<_> = plan.blocks(0).iter().map(|b| b.range.clone()).collect();
        assert_eq!(blocks, [0..16, 16..32, 32..48]);
        let result = run_local(&request, 1);
        let margin = super::pruning_margin("dba", 12);
        // Per receiver: skipped bands, and those the quietest receiver's
        // threshold would have kept.
        let mut skipped = [0usize; 48];
        let mut beyond_quietest = [0usize; 48];
        for record in result["path_diagnostics"].as_array().unwrap() {
            let Some(omitted) = record.get("omitted_energy") else {
                continue;
            };
            // Reflections only: ranked direct paths have their own rule.
            if record["reflection_faces"].as_array().unwrap().is_empty() {
                continue;
            }
            let id = record["receiver_id"].as_str().unwrap();
            let r: usize = id[1..].parse().unwrap();
            for b in 0..8 {
                let bound = omitted[b].as_f64().unwrap();
                if bound == 0. {
                    continue;
                }
                let level = 10. * bound.log10() + weights[b];
                // Never above the receiver's own final threshold.
                assert!(level < uncut[r] - margin + 1e-9, "{id}: band {b} {level}");
                skipped[r] += 1;
                if level >= quietest - margin {
                    beyond_quietest[r] += 1;
                }
            }
        }
        for block in &blocks[1..] {
            // The loud receivers of a later block are pruned, and beyond what
            // the quietest receiver's threshold allows.
            for r in block.clone().filter(|r| r % 3 == 0) {
                assert!(uncut[r] > quietest + 20.);
                assert!(skipped[r] > 0, "r{r} of block {block:?} was not pruned");
                assert!(beyond_quietest[r] > 0, "r{r} of block {block:?}");
            }
            // Its other receivers are pruned too, each within its own level.
            assert!(block.clone().any(|r| r % 3 != 0 && skipped[r] > 0));
        }
    }

    /// The ranking bound is an upper bound: no source delivers more by its
    /// direct family, in any band, at any receiver. Point, line and area
    /// sources, screened and unscreened receivers, a generated grid.
    #[test]
    fn the_direct_bound_is_never_exceeded() {
        use super::Plan;
        let mut request = request();
        request["settings"]["max_reflect_order"] = json!(0);
        let exact = run_local(&request, 4);
        request["settings"]["pruning_strategy"] = json!("dba");
        let cancel = AtomicBool::new(false);
        let plan = Plan::prepare(request, &cancel).unwrap();
        let ranking = plan.ranking.as_ref().unwrap();
        let scene = &plan.scene;
        let (mut pairs, mut loud) = (0, 0);
        for receiver in exact["receivers"].as_array().unwrap() {
            let ri = scene
                .receivers
                .iter()
                .position(|r| receiver["receiver_id"] == r.id.as_str())
                .unwrap();
            for source in receiver["sources"].as_array().unwrap() {
                let si = scene
                    .sources
                    .iter()
                    .position(|s| source["source_id"] == s.id.as_str())
                    .unwrap();
                let bound = ranking.bound(scene, si, ri);
                for b in 0..8 {
                    let Some(level) = source["octave_db"][b].as_f64() else {
                        continue;
                    };
                    let energy = 10f64.powf(level / 10.);
                    assert!(
                        energy <= bound[b] * (1. + 1e-9),
                        "source {si} receiver {ri} band {b}: {energy} above {}",
                        bound[b]
                    );
                    // The bound is not vacuous: unscreened pairs come near it.
                    if energy > 0.05 * bound[b] {
                        loud += 1;
                    }
                }
                pairs += 1;
            }
        }
        assert_eq!(pairs, scene.sources.len() * scene.receivers.len());
        assert!(loud > 0);
    }

    /// The bound keeps a source's directivity, and takes the distance to the
    /// nearest part of an extended source, not to its far end.
    #[test]
    fn the_direct_bound_covers_directivity_and_the_near_end_of_a_source() {
        use super::Plan;
        let row = json!([[20, 20, 20, 20, 20, 20, 20, 20]]);
        let mut request = json!({
            "sources":[
                {"id":"loud","kind":"point","position":[0,0,2],"spectrum":[80,80,80,80,80,80,80,80],
                 "directivity":{"azimuth_deg":[0],"elevation_deg":[-90,90],"correction_db":[row, row]}},
                {"id":"line","kind":"line","vertices":[[0,40,2],[200,40,2]],"spectrum":[80,80,80,80,80,80,80,80]}],
            "receivers":[{"id":"a","position":[0,10,1.5]},{"id":"b","position":[0,42,2]}],
            "settings":{"max_reflect_order":0,"minimum_contribution_db":null}
        });
        let exact = run_local(&request, 2);
        request["settings"]["pruning_strategy"] = json!("dba");
        let plan = Plan::prepare(request, &AtomicBool::new(false)).unwrap();
        let ranking = plan.ranking.as_ref().unwrap();
        for (ri, receiver) in exact["receivers"].as_array().unwrap().iter().enumerate() {
            for source in receiver["sources"].as_array().unwrap() {
                let si = plan
                    .scene
                    .sources
                    .iter()
                    .position(|s| source["source_id"] == s.id.as_str())
                    .unwrap();
                let bound = ranking.bound(&plan.scene, si, ri);
                for b in 0..8 {
                    let energy = 10f64.powf(source["octave_db"][b].as_f64().unwrap() / 10.);
                    assert!(energy <= bound[b] * (1. + 1e-9), "{si} {ri} {b}");
                }
            }
        }
    }

    /// An item's input taken twice is counted once, a floor is kept for the
    /// next item even where the asking source leads, and a lead output
    /// merged twice neither adds to the leaders' energy nor completes
    /// another leader.
    #[test]
    fn repeated_inputs_and_lead_outputs_count_once() {
        use super::{Merge, Plan};
        let cancel = AtomicBool::new(false);
        let plan = Plan::prepare(ranked_request(), &cancel).unwrap();
        let merge = Merge::new(&plan, &|_, _| {}, &|_, _| {});
        let state = || {
            let ranked = merge.ranked.lock().unwrap();
            let lead: Vec<Vec<f64>> = ranked
                .lead
                .iter()
                .map(|bands| bands.iter().map(|e| e.total()).collect())
                .collect();
            (lead, ranked.pending.clone())
        };
        for item in 0..plan.per_stage() {
            let cutoffs = merge.cutoffs(item);
            assert_eq!(merge.cutoffs(item), cutoffs);
            let output = || plan.run_item(item, &cutoffs, &cancel).unwrap();
            merge.add_output(item, output()).unwrap();
            if item == 0 {
                let before = state();
                merge.add_output(item, output()).unwrap();
                assert_eq!(state(), before);
            }
        }
        let leaders: u32 = plan.ranking.as_ref().unwrap().leaders.iter().sum();
        assert_eq!(merge.ranked.lock().unwrap().pairs()[0], leaders as usize);
        // Source 0 leads at some receiver; asking for its item fills the
        // floor at every receiver of the block.
        merge.cutoffs(plan.per_stage());
        let ranked = merge.ranked.lock().unwrap();
        let block = plan.blocks(0)[0].range.clone();
        assert!(
            block
                .clone()
                .any(|ri| plan.ranking.as_ref().unwrap().leads(&plan.scene, 0, ri))
        );
        assert!(block.into_iter().all(|ri| ranked.rest_floor[ri].is_some()));
    }

    /// Three strong sources among the receivers and twenty weak ones far
    /// away, one building, direct paths only.
    fn ranked_request() -> Value {
        let mut sources = vec![];
        for i in 0..3 {
            sources.push(json!({"id":format!("strong{i}"),"kind":"point","position":[10. + 40. * i as f64, 0., 2.],
                "spectrum":[95,98,100,102,105,102,98,94]}));
        }
        for i in 0..20 {
            let y = 400. * (0.3 * i as f64).sin();
            sources.push(json!({"id":format!("weak{i}"),"kind":"point","position":[600. + 30. * i as f64, y, 2.],
                "spectrum":[55,58,60,62,65,62,58,54]}));
        }
        json!({
            "sources":sources,
            "receivers":[{"id":"a","position":[20,15,1.5]},{"id":"b","position":[60,-20,1.5]},
                         {"id":"c","position":[100,30,1.5]},{"id":"d","position":[700,50,1.5]}],
            "buildings":[{"id":"hall","footprint":[[30,5,0],[50,5,0],[50,10,0],[30,10,0]],"height_m":8}],
            "settings":{"max_reflect_order":0,"minimum_contribution_db":null,"pruning_strategy":"dba"}
        })
    }

    /// A receiver ranks nothing out until all of its leaders are merged: an
    /// item of the remaining sources that starts earlier evaluates the
    /// receiver in full, so no item waits and nothing is omitted without the
    /// leaders' energy behind it.
    #[test]
    fn receivers_are_ranked_only_once_their_leaders_are_merged() {
        use super::{EVALUATE, ItemOutput, Merge, OMIT, Plan, SKIP};
        let request = ranked_request();
        let cancel = AtomicBool::new(false);
        let plan = Plan::prepare(request.clone(), &cancel).unwrap();
        let per_stage = plan.per_stage();
        assert_eq!(plan.items(), 2 * per_stage);
        let weak = per_stage + plan.offsets[plan.sources() - 1];
        assert_eq!(plan.stages[plan.locate(weak).0], super::Stage::Rest);
        let add = |merge: &Merge, item: usize| {
            let cutoffs = merge.cutoffs(item);
            let wire =
                serde_json::to_vec(&plan.run_item(item, &cutoffs, &cancel).unwrap()).unwrap();
            let output: ItemOutput = serde_json::from_slice(&wire).unwrap();
            merge.add_output(item, output).unwrap();
            cutoffs.direct.unwrap()
        };
        let weighted = |result: &Value| -> Vec<f64> {
            result["receivers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["a_weighted_db"].as_f64().unwrap())
                .collect()
        };
        let mut uncut = request.clone();
        uncut["settings"]["pruning_strategy"] = Value::Null;
        let uncut = weighted(&run_local(&uncut, 4));
        let local = run_local(&request, 1);
        // In stage order: the weak source is ranked out where others lead.
        let merge = Merge::new(&plan, &|_, _| {}, &|_, _| {});
        let early = merge.cutoffs(weak).direct.unwrap();
        assert!(early.iter().all(|a| *a == EVALUATE || *a == SKIP));
        assert!(early.contains(&EVALUATE));
        for item in 0..per_stage {
            let actions = add(&merge, item);
            assert!(actions.iter().all(|a| *a == EVALUATE || *a == SKIP));
        }
        let mut omitted = 0;
        for item in per_stage..plan.items() {
            let actions = add(&merge, item);
            if item == weak {
                assert!(actions.contains(&OMIT), "{actions:?}");
            }
            omitted += actions.iter().filter(|a| **a == OMIT).count();
        }
        let serial = merge.finish(&cancel).unwrap();
        assert_eq!(serial["receivers"], local["receivers"]);
        let direct = &serial["diagnostics"]["pruning"]["direct"];
        assert_eq!(direct["omitted_pairs"], omitted);
        // The item's first input, taken before any leader was merged, ranked
        // none; only its latest input is counted.
        assert_eq!(
            serial["diagnostics"]["pruning"],
            local["diagnostics"]["pruning"]
        );
        // Remaining sources first: every receiver is evaluated in full, so
        // nothing is omitted and the result is the unpruned one.
        let merge = Merge::new(&plan, &|_, _| {}, &|_, _| {});
        for item in (per_stage..plan.items()).chain(0..per_stage) {
            let actions = add(&merge, item);
            assert!(!actions.contains(&OMIT));
        }
        let eager = merge.finish(&cancel).unwrap();
        assert_eq!(
            eager["diagnostics"]["pruning"]["direct"]["omitted_pairs"],
            0
        );
        assert!(
            eager["diagnostics"]["pruning"]["direct"]["unranked_pairs"]
                .as_u64()
                .unwrap()
                > 0
        );
        for (level, exact) in weighted(&eager).iter().zip(&uncut) {
            assert!((level - exact).abs() < 1e-9, "{level} {exact}");
        }
    }

    /// Zone integration reports are summarized unless a run asks for the
    /// full list; the summary counts every listed report, and both forms
    /// are identical for every division of the work.
    #[test]
    fn integration_reports_are_summarized_unless_requested() {
        let mut request = request();
        request["sources"] = json!([request["sources"][1], request["sources"][2]]);
        let summary = run(&request, 4, 1024);
        let zone_reports = |r: &Value| {
            r["diagnostics"]["integration"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r.get("zone_id").is_some())
                .count()
        };
        assert_eq!(zone_reports(&summary), 0);
        let counted = &summary["diagnostics"]["integration_summary"];
        assert_eq!(counted["reports"], "summary");
        let integrations = counted["zone_integrations"].as_u64().unwrap();
        assert!(integrations > 20);
        assert!(
            counted["largest_relative_discrepancy"]["ratio"]
                .as_f64()
                .unwrap()
                > 0.
        );
        request["output"]["integration_reports"] = json!("full");
        let full = run(&request, 4, 1024);
        assert_eq!(zone_reports(&full) as u64, integrations);
        assert_eq!(
            full["diagnostics"]["integration_summary"]["reports"],
            "full"
        );
        assert_eq!(
            full["receivers"], summary["receivers"],
            "levels do not depend on the report option"
        );
        for (threads, target) in [(1, 1), (8, 7)] {
            assert_eq!(
                run(&request, threads, target),
                full,
                "{threads} threads, plan {target}"
            );
        }
    }

    /// Exact reductions and pair-complete work items: the complete result,
    /// diagnostics included, is bit-identical for every division of the work
    /// and every worker count.
    #[test]
    fn results_do_not_depend_on_work_division_or_thread_count() {
        let request = request();
        let reference = run(&request, 1, 1);
        assert!(reference["receivers"].as_array().unwrap().len() > 20);
        for (threads, target) in [(1, 1024), (4, 1), (4, 6), (4, 1024), (8, 40)] {
            assert_eq!(
                run(&request, threads, target),
                reference,
                "{threads} threads, plan {target}"
            );
        }
        // Path diagnostic records keep one order (point source only: the
        // extended sources' records would exceed the record limit).
        let mut request = request;
        request["sources"] = json!([request["sources"][0]]);
        request["output"]["path_diagnostics"] = json!("summary");
        let reference = run(&request, 1, 1);
        assert!(reference["path_diagnostics"].as_array().unwrap().len() > 20);
        for (threads, target) in [(4, 1024), (8, 3)] {
            assert_eq!(
                run(&request, threads, target),
                reference,
                "{threads} threads, plan {target}"
            );
        }
    }
}
