//! Replaceable inner-loop kernels. The engine runs the scalar kernels below
//! unless a program installs another set before its first calculation. Every
//! installed kernel must decide identically to its scalar version, bit for
//! bit: kernels change speed, never results.
use crate::types::Point;
use std::sync::OnceLock;

/// Lanes in one kernel call.
pub const WIDTH: usize = 8;

/// Up to eight XY boxes in structure-of-arrays form. Lanes from `len` on are
/// unused (empty boxes).
#[derive(Clone, Debug)]
pub struct Boxes {
    pub min: [[f64; WIDTH]; 2],
    pub max: [[f64; WIDTH]; 2],
    pub len: usize,
}

/// An XY query segment from `a` along `d`, widened by `eps` on each axis.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub a: Point,
    pub d: [f64; 2],
    pub eps: f64,
}

/// A kernel set. Each method defaults to the scalar kernel.
pub trait Kernels: Sync {
    /// The kernels this process runs, such as `scalar` or `avx512f`.
    fn name(&self) -> &'static str;
    /// Identity of the kernel source, folded into `build_revision`. It must
    /// not depend on the processor, so farm nodes of one build agree.
    fn revision(&self) -> &'static str;
    /// The lanes whose box meets `segment`, as a bit mask.
    fn box_hits(&self, boxes: &Boxes, segment: &Segment) -> u8 {
        box_hits(boxes, segment)
    }
    /// The lanes whose `robust::orient2d(a, b, (xs, ys))` the floating-point
    /// filter already decides, as a bit mask; `out` holds each certain lane's
    /// exact value. Lanes left out are evaluated with `orient2d`.
    fn filtered_turns(
        &self,
        a: Point,
        b: Point,
        xs: &[f64; WIDTH],
        ys: &[f64; WIDTH],
        out: &mut [f64; WIDTH],
    ) -> u8 {
        let _ = (a, b, xs, ys, out);
        0
    }
}

static INSTALLED: OnceLock<Option<&'static dyn Kernels>> = OnceLock::new();

/// Installs a kernel set for this process. Fails once the engine has chosen
/// its kernels, which it does at its first use of them or of its identity.
pub fn install(kernels: &'static dyn Kernels) -> Result<(), &'static str> {
    INSTALLED
        .set(Some(kernels))
        .map_err(|_| "engine kernels were already chosen")
}

/// The installed kernel set, if any.
pub fn installed() -> Option<&'static dyn Kernels> {
    *INSTALLED.get_or_init(|| None)
}

/// The name of the kernels this process runs.
pub fn name() -> &'static str {
    installed().map_or("scalar", |k| k.name())
}

/// The scalar box test: lane by lane, the slab test along X then Y.
pub fn box_hits(boxes: &Boxes, segment: &Segment) -> u8 {
    (0..boxes.len)
        .filter(|&lane| {
            let min = [boxes.min[0][lane], boxes.min[1][lane]];
            let max = [boxes.max[0][lane], boxes.max[1][lane]];
            box_meets(min, max, segment)
        })
        .fold(0, |mask, lane| mask | 1 << lane)
}

/// Whether the box from `min` to `max` meets the segment.
pub fn box_meets(min: [f64; 2], max: [f64; 2], segment: &Segment) -> bool {
    let Segment { a, d, eps } = *segment;
    let mut lo: f64 = 0.;
    let mut hi: f64 = 1.;
    for k in 0..2 {
        let d = d[k];
        if d == 0. {
            if a[k] < min[k] - eps || a[k] > max[k] + eps {
                return false;
            }
        } else {
            let x = (min[k] - eps - a[k]) / d;
            let y = (max[k] + eps - a[k]) / d;
            lo = lo.max(x.min(y));
            hi = hi.min(x.max(y));
            if lo > hi {
                return false;
            }
        }
    }
    true
}
