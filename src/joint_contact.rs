//! Joint finite-edge contact solver and discrete physical support search.
//! ISO 9613-2:2024 section planes are preserved; images eliminate reflection
//! unknowns. Root exclusion and unresolved branches remain separate outcomes.
use super::*;

#[derive(Clone, Copy, Debug)]
struct ContactInterval {
    lo: f64,
    hi: f64,
}
impl ContactInterval {
    fn point(v: f64) -> Self {
        Self { lo: v, hi: v }
    }
    fn finite(self) -> bool {
        self.lo.is_finite() && self.hi.is_finite() && self.lo <= self.hi
    }
    fn midpoint(self) -> f64 {
        self.lo + (self.hi - self.lo) * 0.5
    }
}
impl std::ops::Add for ContactInterval {
    type Output = Self;
    fn add(self, b: Self) -> Self {
        Self {
            lo: (self.lo + b.lo).next_down(),
            hi: (self.hi + b.hi).next_up(),
        }
    }
}
impl std::ops::Sub for ContactInterval {
    type Output = Self;
    fn sub(self, b: Self) -> Self {
        Self {
            lo: (self.lo - b.hi).next_down(),
            hi: (self.hi - b.lo).next_up(),
        }
    }
}
impl std::ops::Mul for ContactInterval {
    type Output = Self;
    fn mul(self, b: Self) -> Self {
        let p = [
            self.lo * b.lo,
            self.lo * b.hi,
            self.hi * b.lo,
            self.hi * b.hi,
        ];
        if p.iter().any(|v| v.is_nan()) {
            return Self {
                lo: f64::NEG_INFINITY,
                hi: f64::INFINITY,
            };
        }
        Self {
            lo: p.into_iter().fold(f64::INFINITY, f64::min).next_down(),
            hi: p.into_iter().fold(f64::NEG_INFINITY, f64::max).next_up(),
        }
    }
}
impl ContactInterval {
    /// Enclosure of 1/self; `None` when self contains zero.
    fn reciprocal(self) -> Option<Self> {
        if self.lo <= 0. && self.hi >= 0. {
            return None;
        }
        Some(Self {
            lo: (1. / self.hi).next_down(),
            hi: (1. / self.lo).next_up(),
        })
    }
    /// Quotient by a divisor whose `reciprocal` is already known.
    fn over(self, reciprocal: Option<Self>) -> Self {
        match reciprocal {
            Some(r) => self * r,
            None => Self {
                lo: f64::NEG_INFINITY,
                hi: f64::INFINITY,
            },
        }
    }
}
impl std::ops::Div for ContactInterval {
    type Output = Self;
    fn div(self, b: Self) -> Self {
        self.over(b.reciprocal())
    }
}

type I = ContactInterval;
/// Forward-mode gradient. Joint contact systems have few unknowns (rarely
/// more than four); storing them inline avoids a heap allocation on every
/// interval operation, and a small inline size keeps copies cheap.
const INLINE: usize = 4;
type Gradient = smallvec::SmallVec<[I; INLINE]>;
#[derive(Clone)]
struct Dual {
    value: I,
    derivative: Gradient,
}
impl Dual {
    fn constant(value: f64, n: usize) -> Self {
        Self {
            value: I::point(value),
            derivative: smallvec::smallvec![I::point(0.); n],
        }
    }
    fn variable(value: I, n: usize, index: usize) -> Self {
        let mut derivative: Gradient = smallvec::smallvec![I::point(0.); n];
        derivative[index] = I::point(1.);
        Self { value, derivative }
    }
}
impl Dual {
    /// Gradient from component pairs over the shorter gradient, like `zip`.
    fn pairwise(&self, b: &Self, f: impl Fn(I, I) -> I) -> Gradient {
        let n = self.derivative.len().min(b.derivative.len());
        if n <= INLINE {
            // Filled in place: cheaper than pushing through `collect`.
            let mut buffer = [I::point(0.); INLINE];
            for i in 0..n {
                buffer[i] = f(self.derivative[i], b.derivative[i]);
            }
            return Gradient::from_buf_and_len(buffer, n);
        }
        (0..n)
            .map(|i| f(self.derivative[i], b.derivative[i]))
            .collect()
    }
}
// Operations borrow their operands: a gradient is too large to copy per step.
impl std::ops::Add for &Dual {
    type Output = Dual;
    fn add(self, b: &Dual) -> Dual {
        Dual {
            value: self.value + b.value,
            derivative: self.pairwise(b, |a, b| a + b),
        }
    }
}
impl std::ops::Sub for &Dual {
    type Output = Dual;
    fn sub(self, b: &Dual) -> Dual {
        Dual {
            value: self.value - b.value,
            derivative: self.pairwise(b, |a, b| a - b),
        }
    }
}
#[allow(clippy::suspicious_arithmetic_impl)] // Automatic differentiation product rule.
impl std::ops::Mul for &Dual {
    type Output = Dual;
    fn mul(self, b: &Dual) -> Dual {
        Dual {
            value: self.value * b.value,
            derivative: self.pairwise(b, |a, c| a * b.value + self.value * c),
        }
    }
}
#[allow(clippy::suspicious_arithmetic_impl)] // Automatic differentiation quotient rule.
impl std::ops::Div for &Dual {
    type Output = Dual;
    fn div(self, b: &Dual) -> Dual {
        let square = (b.value * b.value).reciprocal();
        Dual {
            value: self.value / b.value,
            derivative: self.pairwise(b, |a, c| (a * b.value - self.value * c).over(square)),
        }
    }
}
macro_rules! by_value {
    ($($trait:ident $method:ident),*) => {$(
        impl std::ops::$trait for Dual {
            type Output = Dual;
            fn $method(self, b: Dual) -> Dual {
                std::ops::$trait::$method(&self, &b)
            }
        }
        impl std::ops::$trait<&Dual> for Dual {
            type Output = Dual;
            fn $method(self, b: &Dual) -> Dual {
                std::ops::$trait::$method(&self, b)
            }
        }
        impl std::ops::$trait<Dual> for &Dual {
            type Output = Dual;
            fn $method(self, b: Dual) -> Dual {
                std::ops::$trait::$method(self, &b)
            }
        }
    )*};
}
by_value!(Add add, Sub sub, Mul mul, Div div);
type DPoint = [Dual; 3];
fn dsub(a: &DPoint, b: &DPoint) -> DPoint {
    std::array::from_fn(|k| &a[k] - &b[k])
}
fn dadd(a: &DPoint, b: &DPoint) -> DPoint {
    std::array::from_fn(|k| &a[k] + &b[k])
}
fn dscale(a: &DPoint, v: &Dual) -> DPoint {
    std::array::from_fn(|k| &a[k] * v)
}
fn ddot(a: &DPoint, b: &DPoint) -> Dual {
    &(&(&a[0] * &b[0]) + &(&a[1] * &b[1])) + &(&a[2] * &b[2])
}

#[derive(Clone)]
struct Height {
    value: Dual,
    coefficients: Vec<Dual>,
}
impl Height {
    fn minus(&self, b: &Self) -> Self {
        Self {
            value: &self.value - &b.value,
            coefficients: self
                .coefficients
                .iter()
                .zip(&b.coefficients)
                .map(|(a, b)| a - b)
                .collect(),
        }
    }
    fn plus(&self, b: &Self) -> Self {
        Self {
            value: &self.value + &b.value,
            coefficients: self
                .coefficients
                .iter()
                .zip(&b.coefficients)
                .map(|(a, b)| a + b)
                .collect(),
        }
    }
    fn times(&self, b: &Dual) -> Self {
        Self {
            value: &self.value * b,
            coefficients: self.coefficients.iter().map(|a| a * b).collect(),
        }
    }
}
struct Branch<'a> {
    scene: &'a Scene,
    s: Point,
    r: Point,
    family: Family,
    edges: Vec<[Point; 2]>,
    counts: Vec<usize>,
    kinds: Vec<u8>,
    scales: Vec<f64>,
}
impl Branch<'_> {
    fn relative(&self, p: Point, n: usize) -> DPoint {
        // Enclose subtraction of original prepared coordinates as well.
        std::array::from_fn(|k| Dual::constant(p[k], n) - Dual::constant(self.s[k], n))
    }
    fn signed(&self, fi: usize, p: &DPoint) -> Dual {
        let f = &self.scene.faces[fi];
        ddot(
            &dsub(p, &self.relative(f.origin, p[0].derivative.len())),
            &std::array::from_fn(|k| Dual::constant(f.normal[k], p[0].derivative.len())),
        )
    }
    /// Mirror contacts between anchors a and b, each one line/plane
    /// intersection of its unfolded endpoints. Chaining contacts through
    /// earlier quotients is algebraically identical but compounds interval
    /// dependency into unbounded enclosures over parameter boxes.
    fn images(&self, a: &DPoint, b: &DPoint, begin: usize, end: usize) -> Vec<DPoint> {
        self.unfolded_ends(a, b, begin, end)
            .into_iter()
            .enumerate()
            .map(|(k, (a, b))| {
                let fi = self.family[begin + k];
                let da = self.signed(fi, &a);
                let t = &da / (&da - self.signed(fi, &b));
                dadd(&a, &dscale(&dsub(&b, &a), &t))
            })
            .collect()
    }
    fn evaluate(&self, x: &[I]) -> (Vec<Dual>, Vec<DPoint>, Vec<usize>) {
        self.evaluate_dual(
            &x.iter()
                .enumerate()
                .map(|(i, v)| Dual::variable(*v, x.len(), i))
                .collect::<Vec<_>>(),
        )
    }
    fn evaluate_dual(&self, x: &[Dual]) -> (Vec<Dual>, Vec<DPoint>, Vec<usize>) {
        let n = x.first().map_or(0, |v| v.derivative.len());
        assert_eq!(x.len(), self.edges.len());
        assert_eq!(self.counts.iter().sum::<usize>(), x.len());
        let values: Vec<_> = self
            .edges
            .iter()
            .zip(x)
            .map(|(e, x)| {
                dadd(
                    &self.relative(e[0], n),
                    &dscale(&dsub(&self.relative(e[1], n), &self.relative(e[0], n)), x),
                )
            })
            .collect();
        let mut anchors = vec![(self.relative(self.s, n), 0usize)];
        let mut at = 0;
        for (section, count) in self.counts.iter().enumerate() {
            for _ in 0..*count {
                anchors.push((values[at].clone(), section));
                at += 1;
            }
        }
        anchors.push((self.relative(self.r, n), self.family.len()));
        let mut points = vec![anchors[0].0.clone()];
        let mut sections = vec![0; self.family.len() + 2];
        let mut next_face = 0;
        for pair in anchors.windows(2) {
            let stop = pair[1].1;
            for (j, p) in self
                .images(&pair[0].0, &pair[1].0, next_face, stop)
                .into_iter()
                .enumerate()
            {
                points.push(p);
                sections[next_face + j + 1] = points.len() - 1;
            }
            points.push(pair[1].0.clone());
            next_face = stop;
        }
        sections[self.family.len() + 1] = points.len() - 1;
        let mut residual = vec![];
        for (section, count) in self.counts.iter().enumerate() {
            let a = &points[sections[section]];
            let b = &points[sections[section + 1]];
            let d = dsub(b, a);
            for p in points.iter().skip(sections[section] + 1).take(*count) {
                let v = dsub(p, a);
                let equation = if self.kinds[section] == 0 {
                    &d[0] * &v[1] - &d[1] * &v[0]
                } else {
                    &v[2] * (&d[0] * &d[0] + &d[1] * &d[1])
                        - &d[2] * (&v[0] * &d[0] + &v[1] * &d[1])
                };
                residual.push(equation / Dual::constant(self.scales[section], n));
            }
        }
        (residual, points, sections)
    }
    /// Propagate corner-height coefficients symbolically. Vertical mirror
    /// images preserve elevation; image interpolation weights depend only on
    /// plan coordinates. This avoids subtracting two wide interval residuals
    /// to recover a small linear coefficient.
    fn height_equations(&self, parameters: &[Dual], linear: &[usize]) -> Vec<Height> {
        let n = parameters.first().map_or(0, |p| p.derivative.len());
        let constant = |p: DPoint| {
            let h = Height {
                value: p[2].clone(),
                coefficients: vec![Dual::constant(0., n); linear.len()],
            };
            (p, h)
        };
        let mut anchors = vec![(constant(self.relative(self.s, n)), 0)];
        let mut at = 0;
        for (section, count) in self.counts.iter().enumerate() {
            for _ in 0..*count {
                let e = self.edges[at];
                let p = dadd(
                    &self.relative(e[0], n),
                    &dscale(
                        &dsub(&self.relative(e[1], n), &self.relative(e[0], n)),
                        &parameters[at],
                    ),
                );
                let (p, mut h) = constant(p);
                if let Some(column) = linear.iter().position(|i| *i == at) {
                    // Unknown is elevation relative to the source, avoiding
                    // loss of a common flat-roof height through interval ratios.
                    h.value = Dual::constant(0., n);
                    h.coefficients[column] = Dual::constant(1., n);
                }
                anchors.push(((p, h), section));
                at += 1;
            }
        }
        anchors.push((constant(self.relative(self.r, n)), self.family.len()));
        let mut points = vec![anchors[0].0.clone()];
        let mut sections = vec![0; self.family.len() + 2];
        let mut next_face = 0;
        for pair in anchors.windows(2) {
            let stop = pair[1].1;
            let mut images = vec![pair[0].0.0.clone()];
            for fi in &self.family[next_face..stop] {
                let normal =
                    std::array::from_fn(|k| Dual::constant(self.scene.faces[*fi].normal[k], n));
                let current = images.last().unwrap();
                images.push(dsub(
                    current,
                    &dscale(
                        &normal,
                        &(Dual::constant(2., n) * self.signed(*fi, current)),
                    ),
                ));
            }
            let mut current = pair[1].0.clone();
            let mut contacts = vec![];
            for k in (next_face..stop).rev() {
                let image = &images[k - next_face + 1];
                let da = self.signed(self.family[k], &current.0);
                let t = &da / (&da - self.signed(self.family[k], image));
                let point = dadd(&current.0, &dscale(&dsub(image, &current.0), &t));
                let height = current
                    .1
                    .times(&(Dual::constant(1., n) - &t))
                    .plus(&pair[0].0.1.times(&t));
                current = (point, height);
                contacts.push(current.clone());
            }
            contacts.reverse();
            for (j, p) in contacts.into_iter().enumerate() {
                points.push(p);
                sections[next_face + j + 1] = points.len() - 1;
            }
            points.push(pair[1].0.clone());
            next_face = stop;
        }
        sections[self.family.len() + 1] = points.len() - 1;
        let mut equations = vec![];
        for (section, count) in self.counts.iter().enumerate() {
            let a = &points[sections[section]];
            let b = &points[sections[section + 1]];
            let d = dsub(&b.0, &a.0);
            for p in points.iter().skip(sections[section] + 1).take(*count) {
                let v = dsub(&p.0, &a.0);
                let projection = &v[0] * &d[0] + &v[1] * &d[1];
                let span = &d[0] * &d[0] + &d[1] * &d[1];
                equations.push(
                    p.1.minus(&a.1)
                        .times(&span)
                        .minus(&b.1.minus(&a.1).times(&projection))
                        .times(&(Dual::constant(1., n) / Dual::constant(self.scales[section], n))),
                );
            }
        }
        equations
    }
    /// Reflect a point through a face plane with the prepared affine image
    /// map, or with its actual inverse. Rounded unit normals make the map
    /// only approximately involutive; the inverse divisor is a constant.
    fn mirror(&self, fi: usize, p: &DPoint, inverse: bool) -> DPoint {
        let n = p[0].derivative.len();
        let normal = std::array::from_fn(|k| Dual::constant(self.scene.faces[fi].normal[k], n));
        let mut factor = Dual::constant(2., n) * self.signed(fi, p);
        if inverse {
            factor =
                factor / (Dual::constant(2., n) * ddot(&normal, &normal) - Dual::constant(1., n));
        }
        dsub(p, &dscale(&normal, &factor))
    }
    /// Unfolded endpoints of every mirror contact between anchors a and b.
    /// For the contact on family[k], `A` is a imaged through the earlier
    /// mirrors and `B` is b mapped back through family[k..end] (this mirror
    /// included) by the inverse image maps. Mirror planes are fixed by those
    /// maps, so the contact where segment A-B meets the plane equals the
    /// chained image construction. Each pair is affine in anchor parameters.
    fn unfolded_ends(
        &self,
        a: &DPoint,
        b: &DPoint,
        begin: usize,
        end: usize,
    ) -> Vec<(DPoint, DPoint)> {
        let mut forward = vec![a.clone()];
        for fi in &self.family[begin..end] {
            let next = self.mirror(*fi, forward.last().unwrap(), false);
            forward.push(next);
        }
        let mut backward = vec![b.clone()];
        for fi in self.family[begin..end].iter().rev() {
            let next = self.mirror(*fi, backward.last().unwrap(), true);
            backward.push(next);
        }
        backward.reverse();
        (0..end - begin)
            .map(|k| (forward[k].clone(), backward[k].clone()))
            .collect()
    }
    /// Prove a box infeasible from each mirror contact's unfolded endpoints,
    /// without dividing by the image-line/plane denominator. A specular contact
    /// lies between A and B, so their signed distances da, db cannot share a
    /// strict sign; a front (back) face needs da >= 0 >= db (da <= 0 <= db).
    /// With the contact C = A + da/(da - db) (B - A), a denominator of proven
    /// sign also lets the homogeneous form C (da - db) test the finite face's
    /// local extent. Unbounded nested quotients cannot hide either proof.
    fn finite_image_bounds(&self, x: &[I]) -> bool {
        let n = 0;
        let mut anchors = vec![(self.relative(self.s, n), 0)];
        let mut at = 0;
        for (section, count) in self.counts.iter().enumerate() {
            for _ in 0..*count {
                let e = self.edges[at];
                let parameter = Dual {
                    value: x[at],
                    derivative: Gradient::new(),
                };
                anchors.push((
                    dadd(
                        &self.relative(e[0], n),
                        &dscale(
                            &dsub(&self.relative(e[1], n), &self.relative(e[0], n)),
                            &parameter,
                        ),
                    ),
                    section,
                ));
                at += 1;
            }
        }
        anchors.push((self.relative(self.r, n), self.family.len()));
        for pair in anchors.windows(2) {
            let (begin, end) = (pair[0].1, pair[1].1);
            for (k, (a, b)) in self
                .unfolded_ends(&pair[0].0, &pair[1].0, begin, end)
                .into_iter()
                .enumerate()
            {
                let fi = self.family[begin + k];
                let face = &self.scene.faces[fi];
                let da = self.signed(fi, &a).value;
                let db = self.signed(fi, &b).value;
                if !da.finite() || !db.finite() {
                    continue;
                }
                if (da.lo > 0. && db.lo > 0.)
                    || (da.hi < 0. && db.hi < 0.)
                    || (face.side == "front" && (da.hi < 0. || db.lo > 0.))
                    || (face.side == "back" && (da.lo > 0. || db.hi < 0.))
                {
                    return false;
                }
                let denominator = da - db;
                let sign = if denominator.lo > 0. {
                    1.
                } else if denominator.hi < 0. {
                    -1.
                } else {
                    continue;
                };
                // C - origin, scaled by the denominator, in face-local axes.
                let origin = self.relative(face.origin, n);
                let scaled: Vec<I> = (0..3)
                    .map(|j| {
                        (a[j].value - origin[j].value) * denominator
                            + (b[j].value - a[j].value) * da
                    })
                    .collect();
                for (axis, local) in [(face.u, 0), (face.v, 1)] {
                    let coordinate = scaled
                        .iter()
                        .zip(axis)
                        .fold(I::point(0.), |acc, (c, w)| acc + *c * I::point(w));
                    let lo = face
                        .projected
                        .iter()
                        .map(|p| p[local])
                        .fold(f64::INFINITY, f64::min)
                        - SUPPORT_EPS_M;
                    let hi = face
                        .projected
                        .iter()
                        .map(|p| p[local])
                        .fold(f64::NEG_INFINITY, f64::max)
                        + SUPPORT_EPS_M;
                    // sign * (coordinate - bound * denominator) has the sign of
                    // (local coordinate - bound) wherever the contact exists.
                    let below = (coordinate - I::point(lo) * denominator) * I::point(sign);
                    let above = (coordinate - I::point(hi) * denominator) * I::point(sign);
                    if (below.finite() && below.hi < 0.) || (above.finite() && above.lo > 0.) {
                        return false;
                    }
                }
            }
        }
        true
    }
    /// Vertical-section contacts share one plan-view line. Use triples of
    /// physical anchors to retain this necessary condition even when image
    /// intersection quotients or height elimination are unbounded.
    fn vertical_anchor_bounds(&self, x: &[I]) -> bool {
        let mut at = 0;
        for (section, count) in self.counts.iter().enumerate() {
            if self.kinds[section] == 0 {
                let mut anchors = vec![];
                if section == 0 {
                    anchors.push(self.relative(self.s, 0));
                }
                for i in at..at + count {
                    let edge = self.edges[i];
                    anchors.push(dadd(
                        &self.relative(edge[0], 0),
                        &dscale(
                            &dsub(&self.relative(edge[1], 0), &self.relative(edge[0], 0)),
                            &Dual {
                                value: x[i],
                                derivative: Gradient::new(),
                            },
                        ),
                    ));
                }
                if section == self.family.len() {
                    anchors.push(self.relative(self.r, 0));
                }
                for triple in anchors.windows(3) {
                    let a = dsub(&triple[1], &triple[0]);
                    let b = dsub(&triple[2], &triple[0]);
                    let cross = a[0].value * b[1].value - a[1].value * b[0].value;
                    if cross.finite() && (cross.lo > 0. || cross.hi < 0.) {
                        return false;
                    }
                }
            }
            at += count;
        }
        true
    }
    /// Necessary reflection predicates only: a box can be rejected when every
    /// point in it violates a physical side or finite-face bound. Interval
    /// uncertainty (including image poles) never establishes geometric absence.
    fn feasible(&self, x: &[I]) -> bool {
        if !self.vertical_anchor_bounds(x) {
            return false;
        }
        // Preserve affine edge/plane correlation before interval coordinates
        // expand it away. A whole adjacent edge on a mirror plane cannot form
        // a nonzero reflection leg, including its numerical boundary strip.
        let mut start: usize = 0;
        for (j, count) in self.counts.iter().enumerate() {
            for fi in [j.checked_sub(1), (j < self.family.len()).then_some(j)]
                .into_iter()
                .flatten()
            {
                let edge = if fi == j {
                    start
                        .checked_add(*count)
                        .and_then(|n| n.checked_sub(1))
                        .filter(|_| *count > 0)
                } else {
                    (*count > 0).then_some(start)
                };
                if let Some(i) = edge {
                    let e = self.edges[i];
                    let a = self
                        .signed(self.family[fi], &self.relative(e[0], self.edges.len()))
                        .value;
                    let b = self
                        .signed(self.family[fi], &self.relative(e[1], self.edges.len()))
                        .value;
                    let signed = a + (b - a) * x[i];
                    if signed.finite() && signed.lo >= -1e-9 && signed.hi <= 1e-9 {
                        return false;
                    }
                }
            }
            start += count;
        }
        let (_, points, sections) = self.evaluate(x);
        if !self.finite_image_bounds(x) {
            return false;
        }
        for (j, fi) in self.family.iter().enumerate() {
            let at = sections[j + 1];
            let f = &self.scene.faces[*fi];
            let a = self.signed(*fi, &points[at - 1]).value;
            let b = self.signed(*fi, &points[at + 1]).value;
            if a.finite() && b.finite() {
                if (a.lo > 0. && b.hi < 0.)
                    || (b.lo > 0. && a.hi < 0.)
                    || (a.lo >= -1e-9 && a.hi <= 1e-9)
                    || (b.lo >= -1e-9 && b.hi <= 1e-9)
                    || (f.side == "front" && (a.hi < 0. || b.hi < 0.))
                    || (f.side == "back" && (a.lo > 0. || b.lo > 0.))
                {
                    return false;
                }
            }
            for k in 0..3 {
                let lo = f
                    .vertices
                    .iter()
                    .map(|p| p[k])
                    .fold(f64::INFINITY, f64::min);
                let hi = f
                    .vertices
                    .iter()
                    .map(|p| p[k])
                    .fold(f64::NEG_INFINITY, f64::max);
                let bound = (I { lo, hi } - I::point(self.s[k]))
                    + I {
                        lo: -SUPPORT_EPS_M,
                        hi: SUPPORT_EPS_M,
                    };
                let contact = points[at][k].value;
                if contact.finite() && (contact.hi < bound.lo || contact.lo > bound.hi) {
                    return false;
                }
            }
        }
        true
    }
    /// Necessary constraints at diffraction anchors, free of image-ray poles.
    /// Vertical anchors: unfold the neighboring physical anchors through the
    /// intervening mirrors; never intersect an image ray with a plane.
    /// Lateral anchors: the section-plane equation with cleared denominators.
    fn unfolded_constraints(&self, parameters: &[Dual]) -> Vec<Dual> {
        let n = parameters.first().map_or(0, |p| p.derivative.len());
        let mut anchors = vec![(self.relative(self.s, n), 0)];
        let mut at = 0;
        for (section, count) in self.counts.iter().enumerate() {
            for _ in 0..*count {
                let edge = self.edges[at];
                anchors.push((
                    dadd(
                        &self.relative(edge[0], n),
                        &dscale(
                            &dsub(&self.relative(edge[1], n), &self.relative(edge[0], n)),
                            &parameters[at],
                        ),
                    ),
                    section,
                ));
                at += 1;
            }
        }
        anchors.push((self.relative(self.r, n), self.family.len()));
        let reflect = |p: DPoint, fi: usize, inverse: bool| {
            let normal = std::array::from_fn(|k| Dual::constant(self.scene.faces[fi].normal[k], n));
            let mut factor = Dual::constant(2., n) * self.signed(fi, &p);
            if inverse {
                // Prepared unit normals are rounded doubles. Use the actual
                // inverse affine map, rather than assume its square is exactly
                // identity. This divisor is constant, not an image-ray pole.
                factor = factor
                    / (Dual::constant(2., n) * ddot(&normal, &normal) - Dual::constant(1., n));
            }
            dsub(&p, &dscale(&normal, &factor))
        };
        let mut constraints = vec![];
        for triple in anchors.windows(3) {
            let section = triple[1].1;
            if self.kinds[section] != 0 {
                continue;
            }
            let mut before = triple[0].0.clone();
            for fi in &self.family[triple[0].1..section] {
                before = reflect(before, *fi, false);
            }
            let mut after = triple[2].0.clone();
            for fi in self.family[section..triple[2].1].iter().rev() {
                after = reflect(after, *fi, true);
            }
            let a = dsub(&before, &triple[1].0);
            let b = dsub(&after, &triple[1].0);
            constraints.push(&a[0] * &b[1] - &a[1] * &b[0]);
        }
        // Lateral anchors: each mirror end A/B of the section is one line/plane
        // intersection between the adjacent anchor and the unfolded neighbor,
        // kept as the polynomial A*dA / B*dB with its denominator dA / dB. The
        // residual Vz*|Dxy|^2 - Dz*(V.Dxy) for V = P - A, D = B - A then scales
        // by dA^3 * dB^2. A real contact has dA, dB != 0, so the zero sets agree
        // there; nested image quotients over a box can be unbounded instead.
        let one = Dual::constant(1., n);
        let contact = |near: &DPoint, far: &DPoint, fi: usize| {
            let dn = self.signed(fi, near);
            let df = self.signed(fi, far);
            let denominator = &dn - df;
            let scaled = dadd(&dscale(near, &denominator), &dscale(&dsub(far, near), &dn));
            (scaled, denominator)
        };
        for (section, kind) in self.kinds.iter().enumerate() {
            if *kind == 0 {
                continue;
            }
            let inside: Vec<_> = (1..anchors.len() - 1)
                .filter(|i| anchors[*i].1 == section)
                .collect();
            let (Some(&first), Some(&last)) = (inside.first(), inside.last()) else {
                continue;
            };
            let previous = &anchors[first - 1];
            let (start, start_scale) = if section == 0 {
                (previous.0.clone(), one.clone())
            } else {
                let mut before = previous.0.clone();
                for fi in &self.family[previous.1..section] {
                    before = reflect(before, *fi, false);
                }
                contact(&anchors[first].0, &before, self.family[section - 1])
            };
            let next = &anchors[last + 1];
            let (end, end_scale) = if section == self.family.len() {
                (next.0.clone(), one.clone())
            } else {
                let mut after = next.0.clone();
                for fi in self.family[section..next.1].iter().rev() {
                    after = reflect(after, *fi, true);
                }
                contact(&anchors[last].0, &after, self.family[section])
            };
            // D * dA * dB and V * dA, all polynomial.
            let d = dsub(&dscale(&end, &start_scale), &dscale(&start, &end_scale));
            for i in inside {
                let v = dsub(&dscale(&anchors[i].0, &start_scale), &start);
                constraints.push(
                    &v[2] * (&d[0] * &d[0] + &d[1] * &d[1])
                        - &d[2] * (&v[0] * &d[0] + &v[1] * &d[1]),
                );
            }
        }
        constraints
    }
    fn fixed_section_heights(&self) -> Vec<Option<f64>> {
        // Vertical images and line/plane intersections preserve a common
        // endpoint elevation exactly, including boxes spanning an image pole.
        // Carry that algebraic identity separately from quotient intervals.
        let mut anchors = vec![(Some(self.s[2]), 0)];
        let mut at = 0;
        for (section, count) in self.counts.iter().enumerate() {
            for _ in 0..*count {
                let edge = self.edges[at];
                anchors.push(((edge[0][2] == edge[1][2]).then_some(edge[0][2]), section));
                at += 1;
            }
        }
        anchors.push((Some(self.r[2]), self.family.len()));
        let mut heights = vec![None; self.family.len() + 2];
        heights[0] = Some(self.s[2]);
        heights[self.family.len() + 1] = Some(self.r[2]);
        for pair in anchors.windows(2) {
            let begin = pair[0].1;
            let end = pair[1].1;
            if pair[0].0.is_some()
                && pair[0].0 == pair[1].0
                && self.family[begin..end]
                    .iter()
                    .all(|fi| self.scene.faces[*fi].normal[2] == 0.)
            {
                for height in &mut heights[begin + 1..end + 1] {
                    *height = pair[0].0;
                }
            }
        }
        heights
    }
    /// A section plane containing a whole straight roof edge has endpoint
    /// vertices, not an isolated sliding vertex along its interior. Canonical
    /// endpoint branches retain those physical vertices without a continuum of
    /// redundant contact parameters. Lateral sections use corner edges at the
    /// endpoints; vertical sections pin the edge parameter at each endpoint.
    fn coincident_edges(&self, x: &[I]) -> Vec<usize> {
        let (_, points, sections) = self.evaluate(x);
        // Section endpoints can depend on every parameter (reflection
        // contacts). The mean-value form keeps that dependence, which a
        // natural enclosure over a wide box loses.
        let middle: Vec<_> = x.iter().map(|v| I::point(v.midpoint())).collect();
        let (_, centre, _) = self.evaluate(&middle);
        let heights = self.fixed_section_heights();
        let mut indices = vec![];
        let mut at = 0;
        for (j, count) in self.counts.iter().enumerate() {
            let a = &points[sections[j]];
            let b = &points[sections[j + 1]];
            let d = dsub(b, a);
            let span = &d[0] * &d[0] + &d[1] * &d[1];
            for _ in 0..*count {
                let e = self.edges[at];
                let fixed_horizontal = heights[j].is_some()
                    && heights[j] == heights[j + 1]
                    && heights[j] == Some(e[0][2])
                    && e[0][2] == e[1][2];
                if self.kinds[j] == 0 && horizontal(e[0], e[1]) > 1e-12 && span.value.lo > 0. {
                    // Distance of both endpoints from the section's vertical
                    // plane, compared without a square root.
                    let limit = SUPPORT_EPS_M * span.value.lo.sqrt();
                    let ca = &centre[sections[j]];
                    let cd = dsub(&centre[sections[j + 1]], ca);
                    let coincident = e.iter().all(|p| {
                        let v = dsub(&self.relative(*p, x.len()), a);
                        let g = &d[0] * &v[1] - &d[1] * &v[0];
                        let cv = dsub(&self.relative(*p, x.len()), ca);
                        let mut centred = (&cd[0] * &cv[1] - &cd[1] * &cv[0]).value;
                        for (k, derivative) in g.derivative.iter().enumerate() {
                            centred = centred + *derivative * (x[k] - middle[k]);
                        }
                        let value = I {
                            lo: g.value.lo.max(centred.lo),
                            hi: g.value.hi.min(centred.hi),
                        };
                        g.value.finite()
                            && centred.finite()
                            && value.lo >= -limit
                            && value.hi <= limit
                    });
                    if coincident {
                        indices.push(at);
                    }
                } else if self.kinds[j] != 0 && horizontal(e[0], e[1]) > 1e-12 && fixed_horizontal {
                    indices.push(at);
                } else if self.kinds[j] != 0 && horizontal(e[0], e[1]) > 1e-12 && span.value.lo > 0.
                {
                    let coincident = e.iter().all(|p| {
                        let v = dsub(&self.relative(*p, x.len()), a);
                        let value =
                            (v[2].clone() - &d[2] * (&v[0] * &d[0] + &v[1] * &d[1]) / &span).value;
                        value.finite() && value.lo >= -SUPPORT_EPS_M && value.hi <= SUPPORT_EPS_M
                    });
                    if coincident {
                        indices.push(at);
                    }
                }
                at += 1;
            }
        }
        indices
    }
    fn root_points(&self, parameters: &[f64]) -> Vec<Point> {
        self.evaluate(&parameters.iter().map(|v| I::point(*v)).collect::<Vec<_>>())
            .1
            .into_iter()
            .map(|p| std::array::from_fn(|k| p[k].value.midpoint() + self.s[k]))
            .collect()
    }
}
/// Intersects a direct enclosure with its mean-value form f(m) + f'(X)(X - m),
/// where `middle` is f evaluated at the box midpoint m. The derivative
/// enclosure f'(X) comes from the direct evaluation. Without a finite
/// mean-value form the direct enclosure is returned unchanged.
fn centered(direct: &Dual, middle: &Dual, x: &[I], m: &[I]) -> I {
    let mut value = middle.value;
    for ((d, x), m) in direct.derivative.iter().zip(x).zip(m) {
        value = value + *d * (*x - *m);
    }
    if !value.finite() {
        return direct.value;
    }
    let lo = direct.value.lo.max(value.lo);
    let hi = direct.value.hi.min(value.hi);
    if lo <= hi { I { lo, hi } } else { direct.value }
}
/// For vertical mirrors, fixed-XY corner heights enter all section equations
/// linearly. Eliminate those variables with interval Gaussian elimination before
/// covering the moving roof/terrain parameters. This is a generic algebraic
/// reduction of the same joint equations, not a separate route algorithm.
struct Reduced<'a, 'scene> {
    branch: &'a Branch<'scene>,
    free: Vec<usize>,
    linear: Vec<usize>,
    /// Supports pinned to a constant edge parameter.
    fixed: Vec<(usize, f64)>,
}
impl<'a, 'scene> Reduced<'a, 'scene> {
    fn new(branch: &'a Branch<'scene>) -> Self {
        let vertical = branch
            .family
            .iter()
            .all(|fi| branch.scene.faces[*fi].normal[2] == 0.);
        let mut free = vec![];
        let mut linear = vec![];
        let mut at = 0;
        for (j, count) in branch.counts.iter().enumerate() {
            for _ in 0..*count {
                let e = branch.edges[at];
                if vertical && branch.kinds[j] != 0 && e[0][0] == e[1][0] && e[0][1] == e[1][1] {
                    linear.push(at);
                } else {
                    free.push(at);
                }
                at += 1;
            }
        }
        Self {
            branch,
            free,
            linear,
            fixed: vec![],
        }
    }
    /// Removes pinned supports from the unknowns. Their equations remain
    /// necessary conditions through `feasible`, never certification rows.
    fn pinned(mut self, pins: &[Option<u8>]) -> Self {
        for (i, pin) in pins.iter().enumerate() {
            if let Some(end) = pin {
                self.free.retain(|f| *f != i);
                self.linear.retain(|f| *f != i);
                self.fixed.push((i, f64::from(*end)));
            }
        }
        self
    }
    fn parameters(&self, x: &[I]) -> Option<Vec<Dual>> {
        let n = x.len();
        let mut parameters = vec![Dual::constant(0., n); self.branch.edges.len()];
        for (i, full) in self.free.iter().enumerate() {
            parameters[*full] = Dual::variable(x[i], n, i);
        }
        for (full, value) in &self.fixed {
            parameters[*full] = Dual::constant(*value, n);
        }
        if self.linear.is_empty() {
            return Some(parameters);
        }
        let equations = self.branch.height_equations(&parameters, &self.linear);
        let m = self.linear.len();
        let row = |equations: &[Height], full: usize| {
            let mut row = equations[full].coefficients.clone();
            row.push(Dual::constant(0., n) - &equations[full].value);
            row
        };
        let mut rows: Vec<Vec<Dual>> = self
            .linear
            .iter()
            .map(|full| row(&equations, *full))
            .collect();
        if x.iter().any(|v| v.lo < v.hi) {
            // Direct evaluation repeats correlated plan quantities (section
            // spans and projections) and loses their dependency: over a small
            // box a row entry can be enclosed ~1000x too wide, and elimination
            // widens it further. The mean-value form of each entry is far
            // tighter on small boxes; both enclose every value on the box.
            let middle: Vec<I> = x.iter().map(|v| I::point(v.midpoint())).collect();
            let mut at_middle = parameters.clone();
            for (i, full) in self.free.iter().enumerate() {
                at_middle[*full] = Dual::variable(middle[i], n, i);
            }
            let centre = self.branch.height_equations(&at_middle, &self.linear);
            for (r, full) in rows.iter_mut().zip(&self.linear) {
                for (entry, middle_entry) in r.iter_mut().zip(row(&centre, *full)) {
                    entry.value = centered(entry, &middle_entry, x, &middle);
                }
            }
        }
        for k in 0..m {
            let pivot = (k..m).max_by(|a, b| {
                rows[*a][k]
                    .value
                    .midpoint()
                    .abs()
                    .total_cmp(&rows[*b][k].value.midpoint().abs())
            })?;
            rows.swap(k, pivot);
            let divisor = rows[k][k].clone();
            if !divisor.value.finite() || (divisor.value.lo <= 0. && divisor.value.hi >= 0.) {
                return None;
            }
            for j in k..=m {
                rows[k][j] = &rows[k][j] / &divisor;
            }
            for i in 0..m {
                if i == k {
                    continue;
                }
                let factor = rows[i][k].clone();
                for j in k..=m {
                    rows[i][j] = &rows[i][j] - &factor * &rows[k][j];
                }
            }
        }
        for (i, full) in self.linear.iter().enumerate() {
            let value = rows[i][m].clone();
            if !value.value.finite() || value.derivative.iter().any(|v| !v.finite()) {
                return None;
            }
            let e = self.branch.edges[*full];
            parameters[*full] = (value + Dual::constant(self.branch.s[2], n)
                - Dual::constant(e[0][2], n))
                / (Dual::constant(e[1][2], n) - Dual::constant(e[0][2], n));
        }
        Some(parameters)
    }
    fn evaluate(&self, x: &[I]) -> Vec<Dual> {
        let Some(parameters) = self.parameters(x) else {
            return vec![
                Dual {
                    value: I {
                        lo: f64::NEG_INFINITY,
                        hi: f64::INFINITY
                    },
                    derivative: smallvec::smallvec![
                        I {
                            lo: f64::NEG_INFINITY,
                            hi: f64::INFINITY
                        };
                        x.len()
                    ]
                };
                x.len()
            ];
        };
        let f = self.branch.evaluate_dual(&parameters).0;
        self.free.iter().map(|i| f[*i].clone()).collect()
    }
    fn feasible(&self, x: &[I], full_domain: &[I]) -> bool {
        // Keep bounded original corner equations available even when an
        // elimination pivot is uncertain. A singular reduced representation
        // cannot hide a proven absence in the original finite-edge system.
        let mut bounded = full_domain.to_vec();
        for (i, full) in self.free.iter().enumerate() {
            bounded[*full] = x[i];
        }
        if !self.branch.feasible(&bounded)
            || self
                .branch
                .evaluate(&bounded)
                .0
                .iter()
                .any(|v| v.value.finite() && (v.value.lo > 0. || v.value.hi < 0.))
        {
            return false;
        }
        let Some(parameters) = self.parameters(x) else {
            return true;
        };
        let bounds: Vec<_> = parameters.iter().map(|v| v.value).collect();
        if bounds
            .iter()
            .zip(full_domain)
            .any(|(p, d)| p.finite() && (p.hi < d.lo || p.lo > d.hi))
        {
            return false;
        }
        self.branch.feasible(&bounds)
    }
    fn constraints(&self, x: &[I], full_domain: &[I]) -> Vec<Dual> {
        // Keep eliminated corner heights bounded in their original domains.
        // No division or elimination is required by these necessary equations.
        let mut parameters: Vec<_> = full_domain
            .iter()
            .map(|value| Dual {
                value: *value,
                derivative: smallvec::smallvec![I::point(0.); x.len()],
            })
            .collect();
        for (i, full) in self.free.iter().enumerate() {
            parameters[*full] = Dual::variable(x[i], x.len(), i);
        }
        for (full, value) in &self.fixed {
            parameters[*full] = Dual::constant(*value, x.len());
        }
        self.branch.unfolded_constraints(&parameters)
    }
    fn width(&self, root: &Root) -> f64 {
        let Some(parameters) = self.parameters(&root.enclosure) else {
            return f64::INFINITY;
        };
        self.branch
            .evaluate_dual(&parameters)
            .1
            .iter()
            .map(|p| {
                (0..3)
                    .map(|k| (p[k].value.hi - p[k].value.lo).powi(2))
                    .sum::<f64>()
                    .sqrt()
            })
            .fold(0., f64::max)
    }
}
fn inverse(matrix: &[Vec<f64>]) -> Option<Vec<Vec<f64>>> {
    let n = matrix.len();
    let mut rows = vec![vec![0.; 2 * n]; n];
    for i in 0..n {
        rows[i][..n].copy_from_slice(&matrix[i]);
        rows[i][n + i] = 1.;
    }
    for k in 0..n {
        let pivot = (k..n).max_by(|a, b| rows[*a][k].abs().total_cmp(&rows[*b][k].abs()))?;
        if !rows[pivot][k].is_finite() || rows[pivot][k].abs() < 1e-18 {
            return None;
        }
        rows.swap(k, pivot);
        let d = rows[k][k];
        for j in 0..2 * n {
            rows[k][j] /= d;
        }
        for i in 0..n {
            if i == k {
                continue;
            }
            let scale = rows[i][k];
            for j in 0..2 * n {
                rows[i][j] -= scale * rows[k][j];
            }
        }
    }
    rows.iter()
        .all(|r| r.iter().all(|v| v.is_finite()))
        .then(|| rows.into_iter().map(|r| r[n..].to_vec()).collect())
}
#[derive(Clone, Debug)]
struct Root {
    enclosure: Vec<I>,
    parameters: Vec<f64>,
}
#[derive(Clone, Debug, Default)]
struct Cover {
    roots: Vec<Root>,
    excluded_boxes: usize,
    unresolved_boxes: usize,
    visited_boxes: usize,
    newton_steps: usize,
    canonical_edges: Vec<usize>,
}
/// Scoped to one fixed source/receiver/reflection transfer. Frequency-specific
/// topology discovery and final eligibility are always repeated per octave.
#[derive(Default)]
pub(super) struct RootCache(std::collections::HashMap<Vec<u64>, Cover>);

fn point_newton(evaluate: &impl Fn(&[I]) -> Vec<Dual>, box_: &[I]) -> (Option<Vec<f64>>, usize) {
    let mut x: Vec<_> = box_.iter().map(|v| v.midpoint()).collect();
    for step in 0..20 {
        let f = evaluate(&x.iter().map(|v| I::point(*v)).collect::<Vec<_>>());
        if !f.iter().all(|v| v.value.finite()) {
            return (None, step);
        }
        let score = f
            .iter()
            .map(|v| v.value.midpoint().abs())
            .fold(0., f64::max);
        if score < 1e-8 {
            return (Some(x), step);
        }
        let jac: Vec<Vec<_>> = f
            .iter()
            .map(|v| v.derivative.iter().map(|d| d.midpoint()).collect())
            .collect();
        let Some(inv) = inverse(&jac) else {
            return (None, step);
        };
        let delta: Vec<_> = inv
            .iter()
            .map(|r| {
                r.iter()
                    .zip(&f)
                    .map(|(c, f)| c * f.value.midpoint())
                    .sum::<f64>()
            })
            .collect();
        let mut accepted = false;
        for halving in 0..20 {
            let scale = 0.5_f64.powi(halving);
            let trial: Vec<_> = x
                .iter()
                .zip(&delta)
                .zip(box_)
                .map(|((v, d), b)| (v - scale * d).clamp(b.lo, b.hi))
                .collect();
            let next = evaluate(&trial.iter().map(|v| I::point(*v)).collect::<Vec<_>>());
            if next.iter().all(|v| v.value.finite())
                && next
                    .iter()
                    .map(|v| v.value.midpoint().abs())
                    .fold(0., f64::max)
                    < score
            {
                x = trial;
                accepted = true;
                break;
            }
        }
        if !accepted {
            return (None, step + 1);
        }
    }
    (None, 20)
}
fn krawczyk(evaluate: &impl Fn(&[I]) -> Vec<Dual>, box_: &[I]) -> Option<Vec<I>> {
    let n = box_.len();
    let f = evaluate(box_);
    if !f
        .iter()
        .all(|v| v.value.finite() && v.derivative.iter().all(|d| d.finite()))
    {
        return None;
    }
    let mid: Vec<_> = box_.iter().map(|v| I::point(v.midpoint())).collect();
    let fm = evaluate(&mid);
    let jac: Vec<Vec<_>> = fm
        .iter()
        .map(|v| v.derivative.iter().map(|d| d.midpoint()).collect())
        .collect();
    let c = inverse(&jac)?;
    let k: Vec<_> = (0..n)
        .map(|i| {
            let mut k = mid[i];
            for (j, v) in fm.iter().enumerate() {
                k = k - I::point(c[i][j]) * v.value;
            }
            for j in 0..n {
                let mut coefficient = I::point(f64::from(i == j));
                for (l, v) in f.iter().enumerate() {
                    coefficient = coefficient - I::point(c[i][l]) * v.derivative[j];
                }
                k = k + coefficient * (box_[j] - mid[j]);
            }
            k
        })
        .collect();
    k.iter().all(|v| v.finite()).then_some(k)
}
fn refine_root(
    evaluate: &impl Fn(&[I]) -> Vec<Dual>,
    root: &mut Root,
    mut complete: impl FnMut(&Root) -> bool,
    max_contractions: usize,
    work: &Work,
) -> Result<bool, Diagnostic> {
    // Contract an already certified root toward the caller's actual goal.
    // Individual parameters may already be at their interval-arithmetic floor;
    // demanding another relative reduction of every width can falsely fail a
    // physically resolved contact. Check the goal after the last step as well.
    for step in 0..=max_contractions {
        work.tick()?;
        if complete(root) {
            let (point, _) = point_newton(evaluate, &root.enclosure);
            if let Some(point) = point {
                root.parameters = point;
                return Ok(true);
            }
            return Ok(false);
        }
        if step == max_contractions {
            return Ok(false);
        }
        let Some(k) = krawczyk(evaluate, &root.enclosure) else {
            return Ok(false);
        };
        let next: Vec<_> = k
            .iter()
            .zip(&root.enclosure)
            .map(|(k, b)| I {
                lo: k.lo.max(b.lo),
                hi: k.hi.min(b.hi),
            })
            .collect();
        if !next.iter().all(|b| b.finite()) {
            return Ok(false);
        }
        if next
            .iter()
            .zip(&root.enclosure)
            .all(|(a, b)| a.lo == b.lo && a.hi == b.hi)
        {
            // No certified progress and the caller's goal remains unmet.
            return Ok(false);
        }
        root.enclosure = next;
    }
    Ok(false)
}
#[cfg(test)]
fn cover(
    evaluate: impl Fn(&[I]) -> Vec<Dual>,
    domain: Vec<I>,
    budget: usize,
    work: &Work,
) -> Result<Cover, Diagnostic> {
    cover_feasible(evaluate, |_| true, domain, budget, work)
}
#[cfg(test)]
fn cover_feasible(
    evaluate: impl Fn(&[I]) -> Vec<Dual>,
    feasible: impl Fn(&[I]) -> bool,
    domain: Vec<I>,
    budget: usize,
    work: &Work,
) -> Result<Cover, Diagnostic> {
    cover_constrained(evaluate, feasible, |_| vec![], domain, budget, work)
}
fn cover_constrained(
    evaluate: impl Fn(&[I]) -> Vec<Dual>,
    feasible: impl Fn(&[I]) -> bool,
    constraints: impl Fn(&[I]) -> Vec<Dual>,
    domain: Vec<I>,
    budget: usize,
    work: &Work,
) -> Result<Cover, Diagnostic> {
    let n = domain.len();
    let mut result = Cover::default();
    let search_domain = domain.clone();
    let mut certificates: Vec<(Vec<I>, Vec<I>)> = vec![];
    let mut queue = vec![domain];
    while let Some(mut box_) = queue.pop() {
        work.tick()?;
        result.visited_boxes += 1;
        if result.visited_boxes > budget {
            result.unresolved_boxes += queue.len() + 1;
            break;
        }
        if certificates
            .iter()
            .any(|(region, _)| contains_box(region, &box_))
        {
            // This region contains at most an already recorded unique root.
            continue;
        }
        if !contract_constraints(&constraints, &mut box_) {
            result.excluded_boxes += 1;
            continue;
        }
        let mut settled = false;
        for _ in 0..8 {
            if !feasible(&box_) {
                result.excluded_boxes += 1;
                settled = true;
                break;
            }
            let f = evaluate(&box_);
            if f.iter()
                .any(|v| v.value.finite() && (v.value.lo > 0. || v.value.hi < 0.))
            {
                result.excluded_boxes += 1;
                settled = true;
                break;
            }
            // The mean-value enclosure retains dependence lost by evaluating
            // a large expression directly. A finite derivative enclosure over
            // the entire box bounds every segment from its midpoint. Either
            // enclosure excluding zero is a proof of absence, not a heuristic.
            // Each finite row can prove absence even if another row has a pole.
            let midpoint: Vec<_> = box_.iter().map(|b| I::point(b.midpoint())).collect();
            let midpoint_values = evaluate(&midpoint);
            let excludes = f.iter().zip(&midpoint_values).any(|(v, m)| {
                if !v.value.finite()
                    || !m.value.finite()
                    || !v.derivative.iter().all(|d| d.finite())
                {
                    return false;
                }
                let mut centered = m.value;
                for ((d, b), c) in v.derivative.iter().zip(&box_).zip(&midpoint) {
                    centered = centered + *d * (*b - *c);
                }
                centered.finite() && (centered.lo > 0. || centered.hi < 0.)
            });
            if excludes {
                result.excluded_boxes += 1;
                settled = true;
                break;
            }
            if !f
                .iter()
                .all(|v| v.value.finite() && v.derivative.iter().all(|d| d.finite()))
            {
                break;
            }
            let Some(krawczyk) = krawczyk(&evaluate, &box_) else {
                break;
            };
            if krawczyk
                .iter()
                .zip(&box_)
                .any(|(k, b)| k.hi < b.lo || k.lo > b.hi)
            {
                result.excluded_boxes += 1;
                settled = true;
                break;
            }
            if krawczyk
                .iter()
                .zip(&box_)
                .all(|(k, b)| k.lo > b.lo && k.hi < b.hi)
            {
                // Strict Krawczyk inclusion proves a unique root on this box.
                // A point approximation is still a proposal, not the proof.
                let known = certificates.iter().any(|(region, root)| {
                    contains_box(region, &krawczyk) || contains_box(&box_, root)
                });
                if known {
                    certificates.push((box_.clone(), krawczyk));
                } else {
                    let (parameters, steps) = point_newton(&evaluate, &krawczyk);
                    result.newton_steps += steps;
                    if let Some(parameters) = parameters {
                        certificates.push((box_.clone(), krawczyk.clone()));
                        result.roots.push(Root {
                            enclosure: krawczyk,
                            parameters,
                        });
                    } else {
                        result.unresolved_boxes += 1;
                    }
                }
                settled = true;
                break;
            }
            let updated: Vec<_> = krawczyk
                .iter()
                .zip(&box_)
                .map(|(k, b)| I {
                    lo: k.lo.max(b.lo),
                    hi: k.hi.min(b.hi),
                })
                .collect();
            let old_width: f64 = box_.iter().map(|b| b.hi - b.lo).sum();
            let new_width: f64 = updated.iter().map(|b| b.hi - b.lo).sum();
            if !updated.iter().all(|v| v.finite()) || new_width >= old_width * 0.9 {
                break;
            }
            box_ = updated;
        }
        if settled {
            continue;
        }
        // A root on a subdivision boundary can evade strict inclusion in both
        // children. Isolate a point proposal inside a verified neighborhood,
        // then cover its complement with disjoint-interior slabs. Neither the
        // proposal nor a failed inclusion can exclude any part of the domain.
        let (proposal, steps) = point_newton(&evaluate, &box_);
        result.newton_steps += steps;
        if let Some(proposal) = proposal {
            if let Some((neighborhood, k)) =
                certified_neighborhood(&evaluate, &box_, &search_domain, &proposal)
            {
                let known = certificates.iter().any(|(region, root)| {
                    contains_box(region, &k) || contains_box(&neighborhood, root)
                });
                certificates.push((neighborhood.clone(), k.clone()));
                if !known {
                    result.roots.push(Root {
                        enclosure: k,
                        parameters: proposal,
                    });
                }
                let mut central = box_.clone();
                for i in 0..n {
                    if central[i].lo < neighborhood[i].lo {
                        let mut left = central.clone();
                        left[i].hi = neighborhood[i].lo;
                        queue.push(left);
                    }
                    if neighborhood[i].hi < central[i].hi {
                        let mut right = central.clone();
                        right[i].lo = neighborhood[i].hi;
                        queue.push(right);
                    }
                    central[i] = I {
                        lo: central[i].lo.max(neighborhood[i].lo),
                        hi: central[i].hi.min(neighborhood[i].hi),
                    };
                }
                continue;
            }
        }
        let split = subdivision_axis(&evaluate, &box_, work)?;
        let middle = box_[split].midpoint();
        // The box budget bounds work. A dimension-independent depth ceiling
        // can stop far above finite-face/contact resolution in coupled boxes.
        // Continue while a representable subdivision and work budget remain.
        if middle <= box_[split].lo || middle >= box_[split].hi {
            result.unresolved_boxes += 1;
            continue;
        }
        let mut right = box_.clone();
        right[split].lo = middle;
        box_[split].hi = middle;
        queue.push(right);
        queue.push(box_);
    }
    Ok(result)
}
/// Interval mean-value/Gauss-Seidel contractor for necessary equations.
/// Every zero in the input box remains in the contracted box. These constraints
/// can prove absence or narrow a domain, but never certify a contact root.
fn contract_constraints(evaluate: &impl Fn(&[I]) -> Vec<Dual>, box_: &mut [I]) -> bool {
    for _ in 0..4 {
        let old_width: f64 = box_.iter().map(|v| v.hi - v.lo).sum();
        let midpoint: Vec<_> = box_.iter().map(|v| I::point(v.midpoint())).collect();
        let values = evaluate(box_);
        let center = evaluate(&midpoint);
        for (f, m) in values.iter().zip(center) {
            if f.value.finite() && (f.value.lo > 0. || f.value.hi < 0.) {
                return false;
            }
            if !m.value.finite() || f.derivative.iter().any(|d| !d.finite()) {
                continue;
            }
            let mut centered = m.value;
            for ((derivative, bound), mid) in f.derivative.iter().zip(box_.iter()).zip(&midpoint) {
                centered = centered + *derivative * (*bound - *mid);
            }
            if centered.finite() && (centered.lo > 0. || centered.hi < 0.) {
                return false;
            }
            for axis in 0..box_.len() {
                let divisor = f.derivative[axis];
                if divisor.lo <= 0. && divisor.hi >= 0. {
                    continue;
                }
                let mut remainder = m.value;
                for (j, derivative) in f.derivative.iter().enumerate() {
                    if j != axis {
                        remainder = remainder + *derivative * (box_[j] - midpoint[j]);
                    }
                }
                let bound = midpoint[axis] - remainder / divisor;
                if !bound.finite() {
                    continue;
                }
                if bound.hi < box_[axis].lo || bound.lo > box_[axis].hi {
                    return false;
                }
                box_[axis].lo = box_[axis].lo.max(bound.lo);
                box_[axis].hi = box_[axis].hi.min(bound.hi);
            }
        }
        let new_width: f64 = box_.iter().map(|v| v.hi - v.lo).sum();
        if new_width >= old_width * 0.9 {
            break;
        }
    }
    true
}
fn contains_box(outer: &[I], inner: &[I]) -> bool {
    outer
        .iter()
        .zip(inner)
        .all(|(a, b)| a.lo <= b.lo && a.hi >= b.hi)
}
fn certified_neighborhood(
    evaluate: &impl Fn(&[I]) -> Vec<Dual>,
    box_: &[I],
    enclosing_box: &[I],
    proposal: &[f64],
) -> Option<(Vec<I>, Vec<I>)> {
    // Preserve the small proposal neighborhood first: a wide interval image
    // can be a poor inflation guide far from a root. Inflation is a fallback
    // for contraction at the outward-rounding floor, never a certificate.
    // It may cross subdivision boundaries inside the original search domain;
    // uniqueness certificates, not overlapping parameter boxes, identify roots.
    for inflate in [false, true] {
        let enclosure = inflate.then(|| krawczyk(evaluate, box_)).flatten();
        let bounds = if inflate { enclosing_box } else { box_ };
        let neighborhood: Vec<_> = box_
            .iter()
            .zip(proposal)
            .enumerate()
            .map(|(i, (b, p))| {
                let rounding_radius = enclosure
                    .as_ref()
                    .map_or(0., |k| 2. * (k[i].lo - p).abs().max((k[i].hi - p).abs()));
                let radius = ((b.hi - b.lo) * 0.02).max(rounding_radius);
                I {
                    lo: (p - radius).max(bounds[i].lo),
                    hi: (p + radius).min(bounds[i].hi),
                }
            })
            .collect();
        if let Some(k) = krawczyk(evaluate, &neighborhood) {
            if k.iter()
                .zip(&neighborhood)
                .all(|(k, b)| k.lo > b.lo && k.hi < b.hi)
            {
                return Some((neighborhood, k));
            }
        }
    }
    None
}
fn subdivision_axis(
    evaluate: &impl Fn(&[I]) -> Vec<Dual>,
    box_: &[I],
    work: &Work,
) -> Result<usize, Diagnostic> {
    let n = box_.len();
    let mut split = (0..n)
        .filter(|i| box_[*i].midpoint() > box_[*i].lo && box_[*i].midpoint() < box_[*i].hi)
        .max_by(|a, b| (box_[*a].hi - box_[*a].lo).total_cmp(&(box_[*b].hi - box_[*b].lo)))
        .unwrap_or(0);
    // A singular interval representation can depend on just one variable.
    // Blindly bisecting every equally wide, unrelated dimension creates an
    // exponential cover before that denominator/pivot becomes bounded.
    // Midpoint probes choose a subdivision axis only; they never exclude a
    // box or certify a root. Both children still cover the original box.
    let finite_score = |f: Vec<Dual>| -> usize {
        f.iter()
            .map(|v| {
                usize::from(v.value.finite()) + v.derivative.iter().filter(|d| d.finite()).count()
            })
            .sum()
    };
    let equations = evaluate(box_);
    let mut score = finite_score(equations.clone());
    if score == n * (n + 1) {
        // Parameter widths alone ignore different edge spans and coupling.
        // Prioritize the largest bounded equation variation J_ij * width_j.
        // This chooses a partition only; both children remain in the cover.
        let impact = |axis: usize| {
            equations
                .iter()
                .map(|f| {
                    let d = f.derivative[axis];
                    d.lo.abs().max(d.hi.abs()) * (box_[axis].hi - box_[axis].lo)
                })
                .fold(0., f64::max)
        };
        let mut largest = impact(split);
        for axis in 0..n {
            let middle = box_[axis].midpoint();
            if middle <= box_[axis].lo || middle >= box_[axis].hi {
                continue;
            }
            let next = impact(axis);
            if next > largest {
                largest = next;
                split = axis;
            }
        }
    } else {
        for axis in 0..n {
            if box_[axis].midpoint() <= box_[axis].lo || box_[axis].midpoint() >= box_[axis].hi {
                continue;
            }
            work.tick()?;
            let mut probe = box_.to_vec();
            probe[axis] = I::point(probe[axis].midpoint());
            let next = finite_score(evaluate(&probe));
            if next > score {
                score = next;
                split = axis;
            }
        }
    }
    Ok(split)
}
#[cfg(test)]
fn unit_domain(n: usize) -> Vec<I> {
    vec![I { lo: 0., hi: 1. }; n]
}

/// Diagnostic code for an incomplete joint search: the routes found are valid
/// but alternatives may be missing. Callers may bound the missing energy by the
/// family cap; otherwise it is reported as `calculation_failed`.
pub(crate) const INCOMPLETE: &str = "contact_search_incomplete";
fn incomplete(message: &str) -> Diagnostic {
    Diagnostic::new(INCOMPLETE, message)
}
const SUPPORT_EPS_M: f64 = 1e-7;
const MAX_SUPPORTS: usize = 16;
const MAX_PLANS: usize = 256;
const ROOT_BOX_BUDGET: usize = 20_000;
#[derive(Clone, Debug)]
struct Edge {
    points: [Point; 2],
    corner: bool,
    lateral: bool,
    vertical: bool,
    terrain: bool,
    faces: Vec<usize>,
}
#[derive(Default)]
pub(crate) struct Catalog {
    edges: Vec<Edge>,
    index: crate::spatial::SpatialIndex,
}
fn point_key(p: Point) -> [u64; 3] {
    p.map(|v| if v == 0. { 0 } else { v.to_bits() })
}
impl Catalog {
    pub(crate) fn prepare(scene: &Scene, cancel: &AtomicBool) -> Result<Self, Diagnostic> {
        let mut entries = std::collections::BTreeMap::<([u64; 3], [u64; 3]), Edge>::new();
        let mut insert = |mut points: [Point; 2],
                          corner: bool,
                          lateral: bool,
                          vertical: bool,
                          terrain: bool,
                          faces: Vec<usize>| {
            if distance(points[0], points[1]) <= 1e-12 {
                return;
            }
            if point_key(points[1]) < point_key(points[0]) {
                points.swap(0, 1);
            }
            let key = (point_key(points[0]), point_key(points[1]));
            entries
                .entry(key)
                .and_modify(|e| {
                    e.corner |= corner;
                    e.lateral |= lateral;
                    e.vertical |= vertical;
                    e.terrain |= terrain;
                    e.faces.extend(&faces);
                    e.faces.sort_unstable();
                    e.faces.dedup();
                })
                .or_insert(Edge {
                    points,
                    corner,
                    lateral,
                    vertical,
                    terrain,
                    faces,
                });
        };
        for b in &scene.buildings {
            check_cancel(cancel)?;
            for (base, roof) in b.base.iter().zip(&b.roof) {
                insert([*base, *roof], true, true, false, false, b.faces.clone());
            }
            for tri in &b.roof_triangles {
                for k in 0..3 {
                    insert(
                        [b.roof[tri[k]], b.roof[tri[(k + 1) % 3]]],
                        false,
                        true,
                        true,
                        false,
                        b.faces.clone(),
                    );
                }
            }
        }
        for (fi, f) in scene.faces.iter().enumerate() {
            check_cancel(cancel)?;
            if scene.building_by_id.contains_key(&f.object) || !f.vertical() {
                continue;
            }
            for k in 0..f.vertices.len() {
                let a = f.vertices[k];
                let b = f.vertices[(k + 1) % f.vertices.len()];
                if horizontal(a, b) <= 1e-12 {
                    insert([a, b], true, true, false, false, vec![fi]);
                } else if vertical_interval(f, a)
                    .is_some_and(|(_, top)| (top - a[2]).abs() <= SUPPORT_EPS_M)
                    && vertical_interval(f, b)
                        .is_some_and(|(_, top)| (top - b[2]).abs() <= SUPPORT_EPS_M)
                {
                    insert([a, b], false, true, true, false, vec![fi]);
                }
            }
        }
        for tri in &scene.terrain.triangles {
            check_cancel(cancel)?;
            for k in 0..3 {
                insert(
                    [
                        scene.terrain.vertices[tri[k]],
                        scene.terrain.vertices[tri[(k + 1) % 3]],
                    ],
                    false,
                    false,
                    true,
                    true,
                    vec![],
                );
            }
        }
        let edges: Vec<_> = entries.into_values().collect();
        let index = crate::spatial::SpatialIndex::new(edges.iter().map(|e| e.points.as_slice()));
        Ok(Self { edges, index })
    }
    fn parameter(e: &Edge, p: Point) -> f64 {
        let d = sub(e.points[1], e.points[0]);
        dot(sub(p, e.points[0]), d) / dot(d, d)
    }
    fn identify(&self, p: Point, kind: u8, a: Point, b: Point) -> Option<(usize, f64)> {
        self.index
            .candidates(p, p)
            .into_iter()
            .filter_map(|i| {
                let e = &self.edges[i];
                if !(if kind == 0 { e.vertical } else { e.lateral })
                    || point_segment_distance(p, e.points[0], e.points[1]) > SUPPORT_EPS_M
                {
                    return None;
                }
                let t = Self::parameter(e, p);
                let d = sub(e.points[1], e.points[0]);
                let leg = sub(b, a);
                let transverse = (d[0] * leg[1] - d[1] * leg[0]).abs();
                let preferred = (kind != 0 && e.corner)
                    || (kind == 0
                        && transverse
                            > 1e-10 * horizontal(e.points[0], e.points[1]) * horizontal(a, b));
                let rank = if preferred { 0 } else { 1 };
                Some((rank, i, t.clamp(0., 1.)))
            })
            .min_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)))
            .map(|(_, i, t)| (i, t))
    }
    fn neighbors(&self, index: usize, end: usize, kind: u8) -> Vec<usize> {
        let point = self.edges[index].points[end];
        self.index
            .candidates(point, point)
            .into_iter()
            .filter(|i| {
                *i != index
                    && (if kind == 0 {
                        self.edges[*i].vertical
                    } else {
                        self.edges[*i].lateral
                    })
                    && self.edges[*i]
                        .points
                        .iter()
                        .any(|p| distance(*p, point) <= SUPPORT_EPS_M)
            })
            .collect()
    }
}
#[derive(Clone)]
struct Plan {
    endpoints: [Point; 2],
    supports: Vec<usize>,
    parameters: Vec<f64>,
    counts: Vec<usize>,
    kinds: Vec<u8>,
    /// Vertical-section supports fixed at an edge endpoint (0 or 1). A section
    /// plane containing the whole edge has only its endpoints as strict hull
    /// vertices; pinning them removes the redundant contact continuum.
    pins: Vec<Option<u8>>,
}
impl Plan {
    fn from_route(scene: &Scene, route: &Route) -> Result<Self, Diagnostic> {
        let mut supports = vec![];
        let mut parameters = vec![];
        let mut counts = vec![];
        for (j, section) in route.sections.windows(2).enumerate() {
            let a = route.points[section[0]];
            let b = route.points[section[1]];
            let mut count = 0;
            for i in section[0] + 1..section[1] {
                if !route.diffraction.contains(&i) {
                    continue;
                }
                let Some((edge, t)) =
                    scene
                        .contact_supports
                        .identify(route.points[i], route.kinds[j], a, b)
                else {
                    return Err(Diagnostic::unsupported(
                        "Joint contact has no identifiable finite physical edge",
                    ));
                };
                supports.push(edge);
                parameters.push(t);
                count += 1;
            }
            counts.push(count);
        }
        if supports.len() > MAX_SUPPORTS {
            return Err(Diagnostic::unsupported(
                "Joint route exceeds the 16-support numerical profile",
            ));
        }
        Ok(Self {
            endpoints: [route.points[0], *route.points.last().unwrap()],
            pins: vec![None; supports.len()],
            supports,
            parameters,
            counts,
            kinds: route.kinds.clone(),
        })
    }
    fn merge_repeated_contacts(&mut self, scene: &Scene) {
        // Within one lateral section the section-height plane assigns exactly
        // one elevation at a fixed XY corner. Repeated copies are the same
        // contact, not independently movable diffraction anchors.
        let mut supports = vec![];
        let mut parameters = vec![];
        let mut pins = vec![];
        let mut at = 0;
        let last = self.counts.len() - 1;
        for (section, count) in self.counts.iter_mut().enumerate() {
            let begin = supports.len();
            for _ in 0..*count {
                let edge = self.supports[at];
                let support = &scene.contact_supports.edges[edge];
                let unique = if self.kinds[section] != 0 {
                    support.corner
                } else {
                    // A vertical section through a fixed endpoint off this
                    // edge's XY line intersects the edge at at most one point.
                    // Keep possible collinear whole-edge intersections intact.
                    [
                        (section == 0).then_some(self.endpoints[0]),
                        (section == last).then_some(self.endpoints[1]),
                    ]
                    .into_iter()
                    .flatten()
                    .any(|p| orient(support.points[0], support.points[1], p) != 0.)
                };
                if let Some(previous) = unique
                    .then(|| supports[begin..].iter().rposition(|i| *i == edge))
                    .flatten()
                {
                    // Returning to the same unique contact makes a closed
                    // loop. A shortest convex lateral route or monotone
                    // vertical profile cannot contain that loop. Retain its
                    // loop-erased support chain for ordinary reconstruction.
                    supports.truncate(begin + previous + 1);
                    parameters.truncate(begin + previous + 1);
                    pins.truncate(begin + previous + 1);
                } else {
                    supports.push(edge);
                    parameters.push(self.parameters[at]);
                    pins.push(self.pins.get(at).copied().flatten());
                }
                at += 1;
            }
            *count = supports.len() - begin;
        }
        self.supports = supports;
        self.parameters = parameters;
        self.pins = pins;
    }
    /// Free vertical-section supports whose edge lies in the section plane for
    /// every parameter value: each section end is a fixed point on the edge's
    /// plan line or a reflection on a vertical face lying on that line. Such a
    /// plan is a contact continuum; only its pinned endpoints are vertices.
    fn collinear_vertical_supports(
        &self,
        scene: &Scene,
        s: Point,
        r: Point,
        family: &Family,
    ) -> Vec<usize> {
        let on_line = |a: Point, b: Point, p: Point| {
            let length = horizontal(a, b);
            length > 1e-12 && orient(a, b, p).abs() <= SUPPORT_EPS_M * length
        };
        let end_on_line = |a: Point, b: Point, end: Result<Point, usize>| match end {
            Ok(p) => on_line(a, b, p),
            Err(fi) => {
                let face = &scene.faces[fi];
                face.vertical() && face.vertices.iter().all(|v| on_line(a, b, *v))
            }
        };
        let last = self.counts.len() - 1;
        let mut out = vec![];
        let mut at = 0;
        for (j, count) in self.counts.iter().enumerate() {
            for _ in 0..*count {
                if self.kinds[j] == 0 && self.pins[at].is_none() {
                    let [a, b] = scene.contact_supports.edges[self.supports[at]].points;
                    let start = if j == 0 { Ok(s) } else { Err(family[j - 1]) };
                    let end = if j == last { Ok(r) } else { Err(family[j]) };
                    if end_on_line(a, b, start) && end_on_line(a, b, end) {
                        out.push(at);
                    }
                }
                at += 1;
            }
        }
        out
    }
    /// A vertical section crossing a straight edge transversally meets it
    /// once; two free copies of that edge are then one contact counted twice,
    /// a singular system. If the section contains the edge, its strict hull
    /// vertices are the two endpoints. Replace the plan by both alternatives.
    fn split_repeated_vertical(&self) -> Option<Vec<Plan>> {
        let mut begin = 0;
        for (j, count) in self.counts.iter().enumerate() {
            let range = begin..begin + count;
            begin += count;
            if self.kinds[j] != 0 {
                continue;
            }
            for a in range.clone() {
                for b in a + 1..range.end {
                    if self.supports[a] != self.supports[b]
                        || self.pins[a].is_some()
                        || self.pins[b].is_some()
                    {
                        continue;
                    }
                    let mut merged = self.clone();
                    merged.supports.remove(b);
                    merged.parameters.remove(b);
                    merged.pins.remove(b);
                    merged.counts[j] -= 1;
                    let mut out = vec![merged];
                    for (first, second) in [(0u8, 1u8), (1, 0)] {
                        let mut pinned = self.clone();
                        pinned.pins[a] = Some(first);
                        pinned.pins[b] = Some(second);
                        pinned.parameters[a] = f64::from(first);
                        pinned.parameters[b] = f64::from(second);
                        out.push(pinned);
                    }
                    return Some(out);
                }
            }
        }
        None
    }
    fn section_of(&self, support: usize) -> Option<usize> {
        let mut end = 0;
        self.counts.iter().position(|count| {
            end += count;
            support < end
        })
    }
    fn key(&self) -> Vec<usize> {
        let pinned = self.pins.iter().any(Option::is_some);
        self.kinds
            .iter()
            .map(|v| *v as usize)
            .chain(self.counts.iter().copied())
            .chain(self.supports.iter().copied())
            .chain(
                pinned
                    .then(|| self.pins.iter().map(|p| p.map_or(0, |e| 1 + e as usize)))
                    .into_iter()
                    .flatten(),
            )
            .collect()
    }
    fn branch<'a>(&self, scene: &'a Scene, s: Point, r: Point, family: &Family) -> Branch<'a> {
        let edges = self
            .supports
            .iter()
            .map(|i| scene.contact_supports.edges[*i].points)
            .collect();
        let mut branch = Branch {
            scene,
            s,
            r,
            family: family.clone(),
            edges,
            counts: self.counts.clone(),
            kinds: self.kinds.clone(),
            scales: vec![1.; self.kinds.len()],
        };
        let (_, points, sections) = branch.evaluate(
            &self
                .parameters
                .iter()
                .map(|p| I::point(*p))
                .collect::<Vec<_>>(),
        );
        for j in 0..self.kinds.len() {
            let a = &points[sections[j]];
            let b = &points[sections[j + 1]];
            let span = (b[0].value.midpoint() - a[0].value.midpoint())
                .hypot(b[1].value.midpoint() - a[1].value.midpoint());
            if span.is_finite() && span > 1e-9 {
                branch.scales[j] = if self.kinds[j] == 0 {
                    span
                } else {
                    span * span
                };
            }
        }
        branch
    }
    fn domain(&self, scene: &Scene) -> Vec<I> {
        // Same finite-edge numerical boundary predicate as geometry checks,
        // measured in metres, independent of edge length and contact tolerance.
        self.supports
            .iter()
            .enumerate()
            .map(|(k, i)| {
                if let Some(end) = self.pins.get(k).copied().flatten() {
                    return I::point(f64::from(end));
                }
                let e = &scene.contact_supports.edges[*i];
                let eps = SUPPORT_EPS_M / distance(e.points[0], e.points[1]);
                I {
                    lo: -eps,
                    hi: 1. + eps,
                }
            })
            .collect()
    }
}
fn skeleton(choices: &[Vec<Point>], family: &Family, kinds: &[u8]) -> Route {
    let mut points = vec![choices[0][0]];
    let mut sections = vec![0];
    let mut reflections = vec![];
    let mut diffraction = vec![];
    for (j, choice) in choices.iter().enumerate() {
        for (i, p) in choice.iter().enumerate().skip(1) {
            points.push(*p);
            if i + 1 < choice.len() {
                diffraction.push(points.len() - 1);
            }
        }
        sections.push(points.len() - 1);
        if j < family.len() {
            reflections.push((points.len() - 1, family[j]));
        }
    }
    Route {
        points,
        sections,
        reflections,
        diffraction,
        kinds: kinds.to_vec(),
        vertical: [0.; 8],
        lateral: [0.; 8],
        terrain_support: false,
    }
}
fn enqueue(
    scene: &Scene,
    route: &Route,
    queue: &mut std::collections::VecDeque<Plan>,
    seen: &mut HashSet<Vec<usize>>,
    work: &Work,
) -> Result<(), Diagnostic> {
    work.tick()?;
    let mut plan = Plan::from_route(scene, route)?;
    plan.merge_repeated_contacts(scene);
    if seen.insert(plan.key()) {
        if seen.len() > MAX_PLANS {
            return Err(incomplete(
                "Joint physical support sequence search limit exceeded",
            ));
        }
        queue.push_back(plan);
    }
    Ok(())
}
fn discover(
    scene: &Scene,
    points: &[Point],
    sections: &[usize],
    kinds: &[u8],
    sides: &[f64],
    family: &Family,
    band: usize,
    offset: f64,
    queue: &mut std::collections::VecDeque<Plan>,
    seen: &mut HashSet<Vec<usize>>,
    work: &Work,
) -> Result<Vec<bool>, Diagnostic> {
    let mut choices = vec![vec![]];
    let mut matched = vec![true; kinds.len()];
    for j in 0..kinds.len() {
        let a = points[sections[j]];
        let b = points[sections[j + 1]];
        let old = &points[sections[j]..=sections[j + 1]];
        let candidates = if kinds[j] == 0 {
            vec![vec![a, b]]
        } else {
            let all = crate::lateral::candidates_on_supports(scene, a, b, band, offset, old, work)?;
            matched[j] = all.iter().any(|p| {
                p.len() == old.len()
                    && p.iter()
                        .zip(old)
                        .all(|(p, q)| distance(*p, *q) <= SUPPORT_EPS_M)
            });
            all.into_iter()
                .filter(|p| p.len() > 2 && lateral_side(p) == sides[j])
                .collect()
        };
        // A blocked provisional lateral section must not suppress discovery of
        // controlling vertical supports on the other side of a reflection.
        // Keep its existing anchors as a discovery proposal only.
        let candidates = if candidates.is_empty() {
            matched[j] = false;
            let finite = old
                .iter()
                .skip(1)
                .take(old.len().saturating_sub(2))
                .all(|p| {
                    scene
                        .contact_supports
                        .identify(*p, kinds[j], a, b)
                        .is_some()
                });
            // An extrapolated proposal can have anchors outside finite edges.
            // Such anchors cannot seed a physical support plan. Keep the
            // section endpoints so other sections can still discover supports;
            // matched=false prevents accepting this placeholder as a route.
            vec![if finite { old.to_vec() } else { vec![a, b] }]
        } else {
            candidates
        };
        let mut next = vec![];
        for prefix in choices {
            for c in &candidates {
                let mut prefix = prefix.clone();
                prefix.push(c.clone());
                next.push(prefix);
                if next.len() > MAX_PLANS {
                    return Err(incomplete("Joint lateral chain discovery limit exceeded"));
                }
            }
        }
        choices = next;
    }
    for selected in choices {
        let raw = skeleton(&selected, family, kinds);
        enqueue(scene, &raw, queue, seen, work)?;
        if let Some(route) = profile_route(
            scene,
            &raw.points,
            &raw.reflections,
            &raw.sections,
            kinds,
            band,
            false,
        )? {
            enqueue(scene, &route, queue, seen, work)?;
        }
    }
    Ok(matched)
}
fn add_corner_endpoints(
    scene: &Scene,
    plan: &Plan,
    index: usize,
    queue: &mut std::collections::VecDeque<Plan>,
    seen: &mut HashSet<Vec<usize>>,
    work: &Work,
) -> Result<(), Diagnostic> {
    let edge = &scene.contact_supports.edges[plan.supports[index]];
    for end in 0..2 {
        for neighbor in scene
            .contact_supports
            .neighbors(plan.supports[index], end, 1)
        {
            if !scene.contact_supports.edges[neighbor].corner {
                continue;
            }
            work.tick()?;
            let mut next = plan.clone();
            next.supports[index] = neighbor;
            next.pins[index] = None;
            next.parameters[index] =
                Catalog::parameter(&scene.contact_supports.edges[neighbor], edge.points[end])
                    .clamp(0., 1.);
            next.merge_repeated_contacts(scene);
            if seen.insert(next.key()) {
                if seen.len() > MAX_PLANS {
                    return Err(incomplete("Joint canonical endpoint search limit exceeded"));
                }
                queue.push_back(next);
            }
        }
    }
    Ok(())
}
/// Replaces a vertical-section support lying in its section plane by its two
/// endpoint vertices; interior points of that straight edge are never strict
/// upper-hull vertices.
fn add_pinned_endpoints(
    plan: &Plan,
    index: usize,
    queue: &mut std::collections::VecDeque<Plan>,
    seen: &mut HashSet<Vec<usize>>,
    work: &Work,
) -> Result<(), Diagnostic> {
    for end in 0..2u8 {
        work.tick()?;
        let mut next = plan.clone();
        next.pins[index] = Some(end);
        next.parameters[index] = f64::from(end);
        if seen.insert(next.key()) {
            if seen.len() > MAX_PLANS {
                return Err(incomplete("Joint canonical endpoint search limit exceeded"));
            }
            queue.push_back(next);
        }
    }
    Ok(())
}
fn add_edge_events(
    scene: &Scene,
    plan: &Plan,
    parameters: &[f64],
    queue: &mut std::collections::VecDeque<Plan>,
    seen: &mut HashSet<Vec<usize>>,
    work: &Work,
) -> Result<(), Diagnostic> {
    let mut at = 0;
    for (j, count) in plan.counts.iter().enumerate() {
        for _ in 0..*count {
            let i = at;
            at += 1;
            let edge = &scene.contact_supports.edges[plan.supports[i]];
            let eps = SUPPORT_EPS_M / distance(edge.points[0], edge.points[1]);
            let ends: Vec<_> = if parameters[i] <= eps {
                vec![0]
            } else if parameters[i] >= 1. - eps {
                vec![1]
            } else {
                vec![]
            };
            for end in ends {
                for neighbor in
                    scene
                        .contact_supports
                        .neighbors(plan.supports[i], end, plan.kinds[j])
                {
                    work.tick()?;
                    let mut next = plan.clone();
                    let point = edge.points[end];
                    next.supports[i] = neighbor;
                    next.pins[i] = None;
                    next.parameters[i] =
                        Catalog::parameter(&scene.contact_supports.edges[neighbor], point)
                            .clamp(0., 1.);
                    next.merge_repeated_contacts(scene);
                    if seen.insert(next.key()) {
                        if seen.len() > MAX_PLANS {
                            return Err(incomplete("Joint edge transition search limit exceeded"));
                        }
                        queue.push_back(next);
                    }
                }
            }
        }
    }
    Ok(())
}
fn vertical_valid(
    scene: &Scene,
    plan: &Plan,
    points: &[Point],
    sections: &[usize],
    band: usize,
) -> bool {
    let mut at = 0;
    for (j, count) in plan.counts.iter().enumerate() {
        if plan.kinds[j] == 0 {
            let p = &points[sections[j]..=sections[j + 1]];
            let trace = cumulative(p);
            let span = horizontal(p[0], *p.last().unwrap());
            let d = sub(*p.last().unwrap(), p[0]);
            let mut previous = 0.;
            for (k, q) in p.iter().enumerate().skip(1).take(p.len() - 2) {
                let along = ((q[0] - p[0][0]) * d[0] + (q[1] - p[0][1]) * d[1]) / span;
                if !along.is_finite()
                    || along < previous - SUPPORT_EPS_M
                    || along > span + SUPPORT_EPS_M
                {
                    return false;
                }
                previous = along;
                let edge = &scene.contact_supports.edges[plan.supports[at + k - 1]];
                if !edge.terrain
                    && !edge
                        .faces
                        .iter()
                        .any(|fi| screen_eligible(scene, *fi, p[0], *p.last().unwrap(), band))
                {
                    return false;
                }
            }
            let hull = upper_hull(
                p.iter()
                    .zip(&trace)
                    .map(|(p, t)| Support {
                        s: *t,
                        z: p[2],
                        terrain: false,
                    })
                    .collect(),
            );
            if hull.len() != p.len() {
                return false;
            }
        }
        at += count;
    }
    true
}
pub(super) fn routes(
    scene: &Scene,
    s: Point,
    r: Point,
    family: &Family,
    reference: &[Point],
    band: usize,
    top: &Option<Route>,
    cache: &mut RootCache,
    work: &Work,
) -> Result<Vec<Route>, Diagnostic> {
    if top.as_ref().is_some_and(|t| t.terrain_support) {
        return Ok(vec![]);
    }
    let dp = horizontal(s, r);
    let offset = top.as_ref().map_or(0., |t| {
        t.points
            .iter()
            .map(|p| {
                let fraction = if dp == 0. {
                    0.
                } else {
                    ((p[0] - s[0]) * (r[0] - s[0]) + (p[1] - s[1]) * (r[1] - s[1])) / (dp * dp)
                };
                (p[2] - s[2] - fraction * (r[2] - s[2])).max(0.) * dp / distance(s, r)
            })
            .fold(0., f64::max)
    });
    let mut combinations = vec![vec![]];
    for leg in reference.windows(2) {
        let mut options = vec![vec![leg[0], leg[1]]];
        options.extend(crate::lateral::candidates(
            scene, leg[0], leg[1], band, offset, work,
        )?);
        let mut next = vec![];
        for prefix in combinations {
            for choice in &options {
                work.tick()?;
                let mut prefix = prefix.clone();
                prefix.push(choice.clone());
                next.push(prefix);
                if next.len() > 10_000 {
                    return Err(incomplete("Mixed route combination limit exceeded"));
                }
            }
        }
        combinations = next;
    }
    let mut results = vec![];
    for selected in combinations {
        let kinds: Vec<_> = selected.iter().map(|p| u8::from(p.len() > 2)).collect();
        if kinds.iter().all(|k| *k == 0) {
            continue;
        }
        let sides: Vec<_> = selected
            .iter()
            .zip(&kinds)
            .map(|(p, k)| if *k == 0 { 0. } else { lateral_side(p) })
            .collect();
        let Some((points, reflections, sections)) =
            resolve_choices(scene, s, r, family, &selected)?
        else {
            continue;
        };
        if family.is_empty() {
            if let Some(route) = build_route(scene, &points, &reflections, &sections, &kinds, band)?
            {
                results.push(route);
            }
            continue;
        }
        let raw = skeleton(
            &sections
                .windows(2)
                .map(|w| points[w[0]..=w[1]].to_vec())
                .collect::<Vec<_>>(),
            family,
            &kinds,
        );
        let mut queue = std::collections::VecDeque::new();
        let mut seen = HashSet::new();
        enqueue(scene, &raw, &mut queue, &mut seen, work)?;
        if let Some(route) =
            profile_route(scene, &points, &reflections, &sections, &kinds, band, false)?
        {
            enqueue(scene, &route, &mut queue, &mut seen, work)?;
        }
        let mut best: Option<Route> = None;
        let mut unresolved = false;
        while let Some(plan) = queue.pop_front() {
            work.tick()?;
            let collinear = plan.collinear_vertical_supports(scene, s, r, family);
            if !collinear.is_empty() {
                for i in collinear {
                    add_pinned_endpoints(&plan, i, &mut queue, &mut seen, work)?;
                }
                continue;
            }
            if let Some(alternatives) = plan.split_repeated_vertical() {
                for next in alternatives {
                    if seen.insert(next.key()) {
                        if seen.len() > MAX_PLANS {
                            return Err(incomplete(
                                "Joint canonical endpoint search limit exceeded",
                            ));
                        }
                        queue.push_back(next);
                    }
                }
                continue;
            }
            let branch = plan.branch(scene, s, r, family);
            let domain = plan.domain(scene);
            if domain.is_empty() {
                continue;
            }
            let reduced = Reduced::new(&branch).pinned(&plan.pins);
            let reduced_domain: Vec<_> = reduced.free.iter().map(|i| domain[*i]).collect();
            // Positive constant row scaling does not change the zero set or
            // a proved root enclosure. Share that geometric cover even when
            // frequency-dependent discovery supplies a different seed span.
            let key: Vec<u64> = plan.key().into_iter().map(|v| v as u64).collect();
            let mut solved = if let Some(covered) = cache.0.get(&key) {
                covered.clone()
            } else {
                let canonical = std::cell::RefCell::new(std::collections::BTreeSet::new());
                let eligible = |x: &[I]| {
                    if !reduced.feasible(x, &domain) {
                        return false;
                    }
                    if let Some(parameters) = reduced.parameters(x) {
                        let bounds: Vec<_> = parameters.iter().map(|p| p.value).collect();
                        let edges: Vec<_> = branch
                            .coincident_edges(&bounds)
                            .into_iter()
                            .filter(|i| plan.pins[*i].is_none())
                            .collect();
                        if !edges.is_empty() {
                            canonical.borrow_mut().extend(edges);
                            return false;
                        }
                    }
                    true
                };
                let mut covered = if reduced.free.is_empty() {
                    if !eligible(&[]) {
                        Cover {
                            excluded_boxes: 1,
                            visited_boxes: 1,
                            ..Cover::default()
                        }
                    } else if reduced.parameters(&[]).is_some() {
                        Cover {
                            roots: vec![Root {
                                enclosure: vec![],
                                parameters: vec![],
                            }],
                            visited_boxes: 1,
                            ..Cover::default()
                        }
                    } else {
                        Cover {
                            unresolved_boxes: 1,
                            visited_boxes: 1,
                            ..Cover::default()
                        }
                    }
                } else {
                    cover_constrained(
                        |x| reduced.evaluate(x),
                        eligible,
                        |x| reduced.constraints(x, &domain),
                        reduced_domain,
                        ROOT_BOX_BUDGET,
                        work,
                    )?
                };
                covered.canonical_edges = canonical.into_inner().into_iter().collect();
                // A cache bound affects speed only, never candidate coverage.
                if cache.0.len() < 4096 {
                    cache.0.insert(key, covered.clone());
                }
                covered
            };
            for i in &solved.canonical_edges {
                if plan.section_of(*i).is_some_and(|j| plan.kinds[j] == 0) {
                    add_pinned_endpoints(&plan, *i, &mut queue, &mut seen, work)?;
                } else {
                    add_corner_endpoints(scene, &plan, *i, &mut queue, &mut seen, work)?;
                }
            }
            unresolved |= solved.unresolved_boxes != 0;
            // Unbounded equations propose finite-edge transition events only.
            // They are never used to accept a path or prove branch absence.
            if let Some(extrapolated) =
                unbounded_proposal(&|x| branch.evaluate(x).0, &plan.parameters)
            {
                if branch.feasible(
                    &extrapolated
                        .iter()
                        .map(|v| I::point(*v))
                        .collect::<Vec<_>>(),
                ) {
                    // An extrapolated zero can have anchors beyond several
                    // finite edges. Recut its sections to discover physical,
                    // ordered supports instead of walking each invalid anchor
                    // independently around the entire connected roof graph.
                    let (points, sections) = {
                        let (_, points, sections) = branch.evaluate(
                            &extrapolated
                                .iter()
                                .map(|v| I::point(*v))
                                .collect::<Vec<_>>(),
                        );
                        let points: Vec<Point> = points
                            .iter()
                            .map(|p| std::array::from_fn(|k| p[k].value.midpoint() + s[k]))
                            .collect();
                        (points, sections)
                    };
                    if points.iter().flatten().all(|v| v.is_finite()) {
                        discover(
                            scene, &points, &sections, &kinds, &sides, family, band, offset,
                            &mut queue, &mut seen, work,
                        )?;
                    }
                }
            }
            for root in &mut solved.roots {
                if !refine_root(
                    &|x| reduced.evaluate(x),
                    root,
                    |root| {
                        let width = reduced.width(root);
                        width.is_finite() && width <= CONTACT_ENCLOSURE_TOLERANCE_M
                    },
                    260, // Previous 20 initial + 12 * 20 extra contractions.
                    work,
                )? {
                    unresolved = true;
                    continue;
                }
                // Corner parameters eliminated from the search must also have
                // resolved finite bounds. Refine toward that classification,
                // without demanding needless shrinkage of unrelated parameters.
                let mut outside = false;
                let bounds_resolved = refine_root(
                    &|x| reduced.evaluate(x),
                    root,
                    |root| {
                        let Some(bounds) = reduced.parameters(&root.enclosure) else {
                            return false;
                        };
                        outside = bounds
                            .iter()
                            .zip(&domain)
                            .any(|(p, d)| p.value.hi < d.lo || p.value.lo > d.hi);
                        outside
                            || bounds
                                .iter()
                                .zip(&domain)
                                .all(|(p, d)| p.value.lo >= d.lo && p.value.hi <= d.hi)
                    },
                    480, // Previous 24 * 20 finite-bound contractions.
                    work,
                )?;
                if outside {
                    // Certified absence needs no successful point approximation.
                    continue;
                }
                if !bounds_resolved {
                    unresolved = true;
                    continue;
                }
                let Some(expanded) = reduced.parameters(
                    &root
                        .parameters
                        .iter()
                        .map(|v| I::point(*v))
                        .collect::<Vec<_>>(),
                ) else {
                    unresolved = true;
                    continue;
                };
                let full_parameters: Vec<_> = expanded.iter().map(|v| v.value.midpoint()).collect();
                let parameters: Vec<_> = full_parameters.iter().map(|v| v.clamp(0., 1.)).collect();
                let points = branch.root_points(&parameters);
                let sections = branch
                    .evaluate(&parameters.iter().map(|v| I::point(*v)).collect::<Vec<_>>())
                    .2;
                if points.iter().flatten().any(|v| !v.is_finite()) {
                    unresolved = true;
                    continue;
                }
                let choices: Vec<_> = sections
                    .windows(2)
                    .map(|w| points[w[0]..=w[1]].to_vec())
                    .collect();
                // Degenerate image roots (a diffraction anchor on the mirror)
                // cannot introduce physical support transitions.
                if resolve_choices(scene, s, r, family, &choices)?.is_none() {
                    continue;
                }
                let matched = discover(
                    scene, &points, &sections, &kinds, &sides, family, band, offset, &mut queue,
                    &mut seen, work,
                )?;
                if matched.iter().any(|v| !*v)
                    || !vertical_valid(scene, &plan, &points, &sections, band)
                {
                    continue;
                }
                add_edge_events(scene, &plan, &full_parameters, &mut queue, &mut seen, work)?;
                let choices: Vec<_> = sections
                    .windows(2)
                    .map(|w| points[w[0]..=w[1]].to_vec())
                    .collect();
                let Some((physical, bounces, indices)) =
                    resolve_choices(scene, s, r, family, &choices)?
                else {
                    continue;
                };
                let Some(route) = build_route(scene, &physical, &bounces, &indices, &kinds, band)?
                else {
                    continue;
                };
                if route.points.len() != points.len()
                    || route.sections != sections
                    || route
                        .points
                        .iter()
                        .zip(&points)
                        .any(|(a, b)| distance(*a, *b) > SUPPORT_EPS_M)
                {
                    continue;
                }
                let wrong: Vec<_> = route
                    .reflections
                    .iter()
                    .filter(|(at, fi)| {
                        !specular_direction_matches(
                            &scene.faces[*fi],
                            route.points[at - 1],
                            route.points[*at],
                            route.points[at + 1],
                        )
                    })
                    .collect();
                if !wrong.is_empty() {
                    // A diffraction-pinned kink rejects a physical candidate.
                    // An otherwise inconsistent image contact is numerical
                    // uncertainty, never evidence for an absent contribution.
                    unresolved |= wrong.iter().any(|(at, _)| !route.diffraction.contains(at));
                    continue;
                }
                if best
                    .as_ref()
                    .is_none_or(|b| length(&route.points) < length(&b.points))
                {
                    best = Some(route);
                }
            }
        }
        if unresolved {
            return Err(incomplete(
                "Joint finite-edge contact search contains unresolved parameter regions",
            ));
        }
        if let Some(route) = best {
            results.push(route);
        }
    }
    Ok(results)
}
fn unbounded_proposal(evaluate: &impl Fn(&[I]) -> Vec<Dual>, seed: &[f64]) -> Option<Vec<f64>> {
    let mut x = seed.to_vec();
    for _ in 0..20 {
        let f = evaluate(&x.iter().map(|p| I::point(*p)).collect::<Vec<_>>());
        if !f.iter().all(|v| v.value.finite()) {
            return None;
        }
        let score = f
            .iter()
            .map(|v| v.value.midpoint().abs())
            .fold(0., f64::max);
        if score < 1e-8 {
            return Some(x);
        }
        let jac: Vec<Vec<_>> = f
            .iter()
            .map(|v| v.derivative.iter().map(|d| d.midpoint()).collect())
            .collect();
        let c = inverse(&jac)?;
        let step: Vec<_> = c
            .iter()
            .map(|row| {
                row.iter()
                    .zip(&f)
                    .map(|(c, f)| c * f.value.midpoint())
                    .sum::<f64>()
            })
            .collect();
        let mut accepted = false;
        for k in 0..20 {
            let scale = 0.5_f64.powi(k);
            let trial: Vec<_> = x.iter().zip(&step).map(|(x, d)| x - scale * d).collect();
            if trial.iter().any(|t| !t.is_finite() || t.abs() > 4.) {
                continue;
            }
            let values = evaluate(&trial.iter().map(|p| I::point(*p)).collect::<Vec<_>>());
            if values.iter().all(|v| v.value.finite())
                && values
                    .iter()
                    .map(|v| v.value.midpoint().abs())
                    .fold(0., f64::max)
                    < score
            {
                x = trial;
                accepted = true;
                break;
            }
        }
        if !accepted {
            return None;
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refinement_meets_physical_goal_when_another_parameter_cannot_shrink_tenfold() {
        // Authored equations with independent exact solution (sqrt(2), 1).
        // Cancellation in (1e6 + 1) - 1e6 creates an outward-rounded floor for
        // y, while the nonlinear x enclosure still needs physical refinement.
        let evaluate = |x: &[I]| {
            let a = Dual::variable(x[0], 2, 0);
            let b = Dual::variable(x[1], 2, 1);
            let one = Dual::constant(1e6, 2) + Dual::constant(1., 2) - Dual::constant(1e6, 2);
            vec![a.clone() * a - Dual::constant(2., 2), b - one]
        };
        let domain = vec![I { lo: 1.3, hi: 1.5 }, I { lo: 0.9, hi: 1.1 }];
        let enclosure = krawczyk(&evaluate, &domain).unwrap();
        assert!(
            enclosure
                .iter()
                .zip(&domain)
                .all(|(a, b)| a.lo > b.lo && a.hi < b.hi)
        );
        let initial = Root {
            enclosure,
            parameters: vec![1.4, 0.99],
        };
        let physical_width = |root: &Root| {
            (100. * (root.enclosure[0].hi - root.enclosure[0].lo))
                .hypot(root.enclosure[1].hi - root.enclosure[1].lo)
        };
        assert!(physical_width(&initial) > CONTACT_ENCLOSURE_TOLERANCE_M);
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        let mut root = initial.clone();
        assert!(
            refine_root(
                &evaluate,
                &mut root,
                |r| physical_width(r) <= CONTACT_ENCLOSURE_TOLERANCE_M,
                20,
                &work
            )
            .unwrap()
        );
        assert!(physical_width(&root) <= CONTACT_ENCLOSURE_TOLERANCE_M);
        for ((bound, point), exact) in root
            .enclosure
            .iter()
            .zip(&root.parameters)
            .zip([2_f64.sqrt(), 1.])
        {
            assert!(bound.lo <= exact && exact <= bound.hi);
            assert!(bound.lo <= *point && *point <= bound.hi);
            assert!(
                (point - exact).abs() < 1e-8,
                "refreshed point: {point} vs {exact}"
            );
        }
        let target = (initial.enclosure[1].hi - initial.enclosure[1].lo) * 0.1;
        assert!(root.enclosure[1].hi - root.enclosure[1].lo > target);
        // An unmet numerical requirement must remain a failure. In production
        // this also guards against accepting an enclosure wider than 1 cm.
        let mut unattainable = initial;
        assert!(
            !refine_root(
                &evaluate,
                &mut unattainable,
                |r| r.enclosure[1].hi - r.enclosure[1].lo <= target,
                20,
                &work
            )
            .unwrap()
        );
    }

    #[test]
    fn refinement_checks_goal_after_last_allowed_contraction() {
        let evaluate = |x: &[I]| vec![Dual::variable(x[0], 1, 0) - Dual::constant(1., 1)];
        let initial = Root {
            enclosure: vec![I { lo: 0.5, hi: 1.5 }],
            parameters: vec![0.75],
        };
        let proof = krawczyk(&evaluate, &initial.enclosure).unwrap();
        assert!(proof[0].lo > 0.5 && proof[0].hi < 1.5);
        let complete = |r: &Root| r.enclosure[0].hi - r.enclosure[0].lo <= 0.01;
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        assert!(!refine_root(&evaluate, &mut initial.clone(), complete, 0, &work).unwrap());
        let mut root = initial;
        assert!(refine_root(&evaluate, &mut root, complete, 1, &work).unwrap());
        assert_eq!(root.parameters, vec![1.]);
        assert!(root.enclosure[0].lo <= 1. && root.enclosure[0].hi >= 1.);
    }
    #[test]
    fn centered_form_tightens_a_dependent_product_and_still_encloses() {
        // f(x) = x (1 - x) on [0.49, 0.51]: the true range is
        // [0.2499, 0.25]. Direct interval evaluation treats both factors as
        // independent ([0.2401, 0.2601]); the mean-value form is
        // f(0.5) + (1 - 2X)(X - 0.5) = 0.25 + [-0.02, 0.02][-0.01, 0.01].
        let f = |x: Dual| x.clone() * (Dual::constant(1., 1) - x);
        let x = [I { lo: 0.49, hi: 0.51 }];
        let m = [I::point(0.5)];
        let direct = f(Dual::variable(x[0], 1, 0));
        let middle = f(Dual::variable(m[0], 1, 0));
        let tight = centered(&direct, &middle, &x, &m);
        assert!(tight.lo <= 0.2499 && tight.hi >= 0.25, "{tight:?}");
        assert!(tight.lo >= 0.2497 && tight.hi <= 0.2503, "{tight:?}");
        assert!(direct.value.lo < 0.2402 && direct.value.hi > 0.26);
        // An unbounded derivative leaves the direct enclosure unchanged.
        let mut unbounded = direct.clone();
        unbounded.derivative[0] = I {
            lo: f64::NEG_INFINITY,
            hi: f64::INFINITY,
        };
        let kept = centered(&unbounded, &middle, &x, &m);
        assert_eq!((kept.lo, kept.hi), (direct.value.lo, direct.value.hi));
    }
    #[test]
    fn joint_corner_matches_independent_affine_solution_at_both_coordinate_scales() {
        for offset in [[0.; 3], [600000., 300000., 200.]] {
            let shift = |p: Point| add(p, offset);
            let cancel = AtomicBool::new(false);
            let operations = AtomicUsize::new(0);
            let work = Work {
                cancel: &cancel,
                operations: &operations,
            };
            let scene=Scene::prepare(&json!({"reflectors":[{"id":"wall","vertices":[shift([0.,-100.,0.]),shift([0.,100.,0.]),shift([0.,100.,30.]),shift([0.,-100.,30.])]}]}),&cancel).unwrap();
            let branch = Branch {
                scene: &scene,
                s: shift([10., -10., 6.]),
                r: shift([10., 10., 1.]),
                family: vec![0],
                edges: vec![[shift([2., 0., 0.]), shift([2., 0., 20.])]],
                counts: vec![0, 1],
                kinds: vec![0, 1],
                scales: vec![1., 425.],
            };
            let result = cover(|x| branch.evaluate(x).0, unit_domain(1), 2000, &work).unwrap();
            assert_eq!(result.unresolved_boxes, 0, "{result:?}");
            assert_eq!(result.roots.len(), 1, "{result:?}");
            let root = &result.roots[0];
            let exact_t = 85. / 520.;
            assert!(root.enclosure[0].lo <= exact_t && exact_t <= root.enclosure[0].hi);
            let points = branch.root_points(&root.parameters);
            for (actual, expected) in points.iter().zip([
                shift([10., -10., 6.]),
                shift([0., -5. / 3., 581. / 156.]),
                shift([2., 0., 85. / 26.]),
                shift([10., 10., 1.]),
            ]) {
                assert!(
                    distance(*actual, expected) < 1e-8,
                    "{actual:?} {expected:?}"
                );
            }
        }
    }
    #[test]
    fn physical_domain_excludes_degenerate_mirror_edges_without_solving_poles() {
        for offset in [[0.; 3], [600000., 300000., 200.]] {
            let shift = |p: Point| add(p, offset);
            let cancel = AtomicBool::new(false);
            let operations = AtomicUsize::new(0);
            let work = Work {
                cancel: &cancel,
                operations: &operations,
            };
            let scene = Scene::prepare(&json!({"reflectors":[{"id":"mirror",
                "vertices":[shift([0.,-100.,0.]),shift([0.,100.,0.]),shift([0.,100.,30.]),shift([0.,-100.,30.])]}]}), &cancel).unwrap();
            let mut branch = Branch {
                scene: &scene,
                s: shift([10., -10., 6.]),
                r: shift([10., 10., 1.]),
                family: vec![0],
                edges: vec![[shift([0., -1., 0.]), shift([0., -1., 30.])]],
                counts: vec![1, 0],
                kinds: vec![1, 0],
                scales: vec![100., 1.],
            };
            let result = cover_feasible(
                |x| branch.evaluate(x).0,
                |x| branch.feasible(x),
                unit_domain(1),
                2000,
                &work,
            )
            .unwrap();
            assert_eq!(result.excluded_boxes, 1);
            assert_eq!(result.unresolved_boxes, 0);
            assert!(result.roots.is_empty());
            branch.edges = vec![[shift([1., -1., 0.]), shift([1., -1., 30.])]];
            let result = cover_feasible(
                |x| branch.evaluate(x).0,
                |x| branch.feasible(x),
                unit_domain(1),
                2000,
                &work,
            )
            .unwrap();
            assert_eq!(result.unresolved_boxes, 0);
            assert_eq!(result.roots.len(), 1);
        }
    }

    #[test]
    fn interval_cover_distinguishes_unique_absent_multiple_and_uncertain_roots() {
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        let linear = |x: &[I]| {
            vec![
                Dual::variable(x[0], 2, 0) - Dual::constant(0.2, 2),
                Dual::variable(x[1], 2, 1) - Dual::constant(0.7, 2),
            ]
        };
        let result = cover(linear, unit_domain(2), 2000, &work).unwrap();
        assert_eq!(result.roots.len(), 1);
        assert_eq!(result.unresolved_boxes, 0);
        let absent = cover(
            |x| vec![Dual::variable(x[0], 1, 0) + Dual::constant(1., 1)],
            unit_domain(1),
            2000,
            &work,
        )
        .unwrap();
        assert!(absent.roots.is_empty());
        assert_eq!(absent.unresolved_boxes, 0);
        assert_eq!(absent.excluded_boxes, 1);
        let multiple = cover(
            |x| {
                let v = Dual::variable(x[0], 1, 0);
                vec![(v.clone() - Dual::constant(0.2, 1)) * (v - Dual::constant(0.8, 1))]
            },
            unit_domain(1),
            2000,
            &work,
        )
        .unwrap();
        assert_eq!(multiple.roots.len(), 2, "{multiple:?}");
        assert_eq!(multiple.unresolved_boxes, 0, "{multiple:?}");
        let singular = cover(
            |x| {
                let v = Dual::variable(x[0], 1, 0);
                vec![v.clone() * v]
            },
            unit_domain(1),
            2000,
            &work,
        )
        .unwrap();
        assert!(
            singular.unresolved_boxes > 0,
            "boundary/singular root must not become an absence: {singular:?}"
        );
        let limited = cover(linear, unit_domain(2), 0, &work).unwrap();
        assert!(limited.unresolved_boxes > 0);
        let pole = cover(
            |x| vec![Dual::constant(1., 1) / (Dual::variable(x[0], 1, 0) - Dual::constant(0.5, 1))],
            unit_domain(1),
            20,
            &work,
        )
        .unwrap();
        assert!(
            pole.unresolved_boxes > 0,
            "a singular image denominator must remain uncertain"
        );
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            cover(linear, unit_domain(2), 2000, &work).unwrap_err().code,
            "cancelled"
        );
    }

    #[test]
    fn repeated_contacts_merge_only_when_the_section_intersection_is_unique() {
        let cancel = AtomicBool::new(false);
        let scene = Scene::prepare(
            &json!({"buildings":[{
                "id":"box", "footprint":[[2.,1.,0.],[4.,1.,0.],[4.,3.,0.],[2.,3.,0.]],"height_m":4.
            }]}),
            &cancel,
        )
        .unwrap();
        let corner = scene
            .contact_supports
            .edges
            .iter()
            .position(|e| e.corner)
            .unwrap();
        let roof = scene
            .contact_supports
            .edges
            .iter()
            .position(|e| e.vertical && e.points[0][1] == 3. && e.points[1][1] == 3.)
            .unwrap();
        let original = Plan {
            endpoints: [[10., -10., 6.], [10., 10., 1.]],
            supports: vec![corner, corner, roof, roof],
            parameters: vec![0.3, 0.3, 0.6, 0.6],
            counts: vec![2, 2],
            kinds: vec![1, 0],
            pins: vec![None; 4],
        };
        let mut plan = original.clone();
        plan.merge_repeated_contacts(&scene);
        assert_eq!(plan.supports, vec![corner, roof]);
        assert_eq!(plan.counts, vec![1, 1]);
        assert_eq!(plan.parameters, vec![0.3, 0.6]);
        let other = scene
            .contact_supports
            .edges
            .iter()
            .enumerate()
            .find(|(i, e)| *i != corner && e.corner)
            .unwrap()
            .0;
        let mut looped = Plan {
            endpoints: plan.endpoints,
            supports: vec![corner, other, corner, roof],
            parameters: vec![0.3, 0.5, 0.3, 0.6],
            counts: vec![3, 1],
            kinds: vec![1, 0],
            pins: vec![None; 4],
        };
        looped.merge_repeated_contacts(&scene);
        assert_eq!(
            looped.supports,
            vec![corner, roof],
            "closed contact loop is not a shortest section"
        );
        let mut collinear = original;
        collinear.endpoints[1] = [10., 3., 1.];
        collinear.merge_repeated_contacts(&scene);
        assert_eq!(
            collinear.counts,
            vec![1, 2],
            "whole-edge intersection must remain available"
        );
        let mut separate = Plan {
            endpoints: plan.endpoints,
            supports: vec![corner, corner],
            parameters: vec![0.3, 0.7],
            counts: vec![1, 1],
            kinds: vec![1, 1],
            pins: vec![None; 2],
        };
        separate.merge_repeated_contacts(&scene);
        assert_eq!(
            separate.counts,
            vec![1, 1],
            "a reflection separates independent contacts"
        );
    }

    #[test]
    fn root_certificate_encloses_rounding_floor_without_relaxing_inclusion() {
        // Exact authored equation: (10000+x)-10000-1/2=0 has one root, x=1/2.
        // Its natural interval evaluation loses precision through cancellation.
        // A contracted box narrower than that arithmetic floor cannot satisfy
        // strict inclusion, even though its original search box can.
        let evaluate = |x: &[I]| {
            vec![
                Dual::constant(10000., 1) + Dual::variable(x[0], 1, 0)
                    - Dual::constant(10000., 1)
                    - Dual::constant(0.5, 1),
            ]
        };
        let tight = vec![I {
            lo: 0.5 - 1e-14,
            hi: 0.5 + 1e-14,
        }];
        let k = krawczyk(&evaluate, &tight).unwrap();
        assert!(k[0].lo < tight[0].lo && k[0].hi > tight[0].hi);
        let original = unit_domain(1);
        let (neighborhood, root) =
            certified_neighborhood(&evaluate, &tight, &original, &[0.5]).unwrap();
        assert!(neighborhood[0].lo >= 0. && neighborhood[0].hi <= 1.);
        assert!(root[0].lo <= 0.5 && root[0].hi >= 0.5);
        assert!(root[0].lo > neighborhood[0].lo && root[0].hi < neighborhood[0].hi);
        // No room in the original box means no certificate, not an accepted
        // approximate root or an exclusion of its unresolved neighborhood.
        assert!(certified_neighborhood(&evaluate, &tight, &tight, &[0.5]).is_none());
    }

    #[test]
    fn centered_bounds_exclude_dependency_without_inverting_a_singular_jacobian() {
        // Authored exact function x-x+1/4 has no zero and derivative zero.
        // Natural evaluation over [0,1] includes zero through dependency;
        // a midpoint/derivative enclosure proves positivity on the whole box.
        let evaluate = |x: &[I]| {
            vec![Dual {
                value: x[0] - x[0] + I::point(0.25),
                derivative: smallvec::smallvec![I::point(0.)],
            }]
        };
        let domain = unit_domain(1);
        let f = evaluate(&domain);
        assert!(f[0].value.lo < 0. && f[0].value.hi > 0.);
        assert!(krawczyk(&evaluate, &domain).is_none());
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        let result = cover(evaluate, domain, 1, &work).unwrap();
        assert_eq!(result.visited_boxes, 1, "{result:?}");
        assert_eq!(result.excluded_boxes, 1);
        assert_eq!(result.unresolved_boxes, 0);
        assert!(result.roots.is_empty());
    }

    #[test]
    fn finite_equation_proves_absence_despite_another_equations_pole() {
        let evaluate = |x: &[I], constant| {
            vec![
                Dual::constant(1., 2) / (Dual::variable(x[0], 2, 0) - Dual::constant(0.5, 2)),
                Dual {
                    value: x[1] - x[1] + I::point(constant),
                    derivative: smallvec::smallvec![I::point(0.); 2],
                },
            ]
        };
        let domain = unit_domain(2);
        let f = evaluate(&domain, 0.25);
        assert!(!f[0].value.finite());
        assert!(f[1].value.lo < 0. && f[1].value.hi > 0.);
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        // The second exact equation is the positive constant 1/4. It alone
        // proves there is no joint zero, regardless of the first row's pole.
        let absent = cover(|x| evaluate(x, 0.25), domain.clone(), 1, &work).unwrap();
        assert_eq!(absent.visited_boxes, 1);
        assert_eq!(absent.excluded_boxes, 1);
        assert_eq!(absent.unresolved_boxes, 0);
        assert!(absent.roots.is_empty());
        // A zero second row supplies no such proof. An unresolved pole cannot
        // be silently classified as absent when the work budget is exhausted.
        let uncertain = cover(|x| evaluate(x, 0.), domain, 1, &work).unwrap();
        assert!(uncertain.unresolved_boxes > 0);
        assert!(uncertain.roots.is_empty());
    }

    #[test]
    fn root_on_subdivision_boundary_is_certified_and_counted_once() {
        // Exact authored system: y=1/4, then (x-1/2)*(y-1/2)=0 gives x=1/2.
        // The initial midpoint Jacobian is singular. The longest-axis split
        // puts the unique nonsingular root on the shared x=1/2 boundary.
        let evaluate = |x: &[I]| {
            let a = Dual::variable(x[0], 2, 0) - Dual::constant(0.5, 2);
            let b = Dual::variable(x[1], 2, 1);
            vec![
                a * (b.clone() - Dual::constant(0.5, 2)),
                b - Dual::constant(0.25, 2),
            ]
        };
        let domain = vec![I { lo: -1.5, hi: 2.5 }, I { lo: 0., hi: 1. }];
        assert!(krawczyk(&evaluate, &domain).is_none());
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        assert_eq!(subdivision_axis(&evaluate, &domain, &work).unwrap(), 0);
        let result = cover(evaluate, domain, 128, &work).unwrap();
        assert_eq!(result.unresolved_boxes, 0, "{result:?}");
        assert_eq!(result.roots.len(), 1, "{result:?}");
        for (enclosure, expected) in result.roots[0].enclosure.iter().zip([0.5, 0.25]) {
            assert!(enclosure.lo <= expected && enclosure.hi >= expected);
        }
    }

    #[test]
    fn finite_equation_variation_guides_subdivision_without_changing_roots() {
        // Exact authored system: y=3/4, x-1/4+100*(y-3/4)^3=0,
        // hence the unique root is (1/4,3/4). Equal parameter widths hide
        // the much larger equation variation caused by the second parameter.
        let evaluate = |x: &[I]| {
            let a = Dual::variable(x[0], 2, 0) - Dual::constant(0.25, 2);
            let b = Dual::variable(x[1], 2, 1) - Dual::constant(0.75, 2);
            vec![
                a + Dual::constant(100., 2) * b.clone() * b.clone() * b.clone(),
                b,
            ]
        };
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        let domain = unit_domain(2);
        assert_eq!(subdivision_axis(&evaluate, &domain, &work).unwrap(), 1);
        let result = cover(evaluate, domain, 128, &work).unwrap();
        assert_eq!(result.unresolved_boxes, 0, "{result:?}");
        assert_eq!(result.roots.len(), 1, "{result:?}");
        for (b, expected) in result.roots[0].enclosure.iter().zip([0.25, 0.75]) {
            assert!(b.lo <= expected && b.hi >= expected);
        }
    }

    #[test]
    fn root_cover_uses_remaining_budget_past_forty_bisections() {
        // Exact residual is the positive constant 1e-14, so there is no root.
        // Natural interval evaluation of (x+1e-14)-x requires a narrow box.
        // A necessary domain predicate keeps only the tiny near-zero region;
        // proving its absence needs >40 bisections but far fewer than 128 boxes.
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        let result = cover_feasible(
            |x| {
                vec![Dual {
                    value: x[0] + I::point(1e-14) - x[0],
                    // A conservative derivative enclosure contains the exact
                    // zero derivative but cannot shortcut the deep cover.
                    derivative: smallvec::smallvec![I { lo: -1., hi: 1. }],
                }]
            },
            |x| x[0].lo <= 1e-14,
            unit_domain(1),
            128,
            &work,
        )
        .unwrap();
        assert_eq!(result.unresolved_boxes, 0, "{result:?}");
        assert!(result.roots.is_empty());
        assert!(
            result.visited_boxes > 80 && result.visited_boxes <= 128,
            "{result:?}"
        );
    }

    #[test]
    fn interval_pivot_uncertainty_splits_its_responsible_parameter() {
        // Independently exact system: every x_i=1/4. The denominator is
        // identically 1, but natural interval evaluation of x0-x0+1 contains
        // zero on [0,1]. Seven unrelated coordinates must not consume the
        // cover budget before the responsible x0 interval is subdivided.
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        let evaluate = |x: &[I]| {
            let n = x.len();
            let variables: Vec<_> = x
                .iter()
                .enumerate()
                .map(|(i, x)| Dual::variable(*x, n, i))
                .collect();
            let mut f: Vec<_> = variables
                .iter()
                .map(|v| v.clone() - Dual::constant(0.25, n))
                .collect();
            f[0] = f[0].clone()
                / (variables[0].clone() - variables[0].clone() + Dual::constant(1., n));
            f
        };
        assert_eq!(
            subdivision_axis(&evaluate, &unit_domain(8), &work).unwrap(),
            0
        );
        let result = cover(evaluate, unit_domain(8), 64, &work).unwrap();
        assert_eq!(result.unresolved_boxes, 0, "{result:?}");
        assert_eq!(result.roots.len(), 1, "{result:?}");
        for (bound, point) in result.roots[0]
            .enclosure
            .iter()
            .zip(&result.roots[0].parameters)
        {
            assert!(bound.lo <= 0.25 && bound.hi >= 0.25);
            assert!((point - 0.25).abs() < 1e-12);
        }
    }

    #[test]
    fn finite_faces_exclude_image_poles_without_division() {
        // Independent image construction: S=(1,0,1) reflects through x=0,
        // x=20 to S''=(41,0,1). A last anchor at (41,2,1) makes the image
        // line parallel to x=20, so it cannot reach that finite wall. The
        // same remains true for x in [40.999,41.001] with |wall y| <= 100.
        for offset in [[0.; 3], [600000., 300000., 200.]] {
            let shift = |p: Point| add(p, offset);
            let cancel = AtomicBool::new(false);
            let operations = AtomicUsize::new(0);
            let work = Work {
                cancel: &cancel,
                operations: &operations,
            };
            let scene = Scene::prepare(&json!({"reflectors":[
                {"id":"a","vertices":[shift([0.,-100.,0.]),shift([0.,100.,0.]),shift([0.,100.,30.]),shift([0.,-100.,30.])]},
                {"id":"b","vertices":[shift([20.,-100.,0.]),shift([20.,100.,0.]),shift([20.,100.,30.]),shift([20.,-100.,30.])]}
            ]}), &cancel).unwrap();
            let mut branch = Branch {
                scene: &scene,
                s: shift([1., 0., 1.]),
                r: shift([10., 10., 1.]),
                family: vec![0, 1],
                edges: vec![[shift([40.999, 2., 1.]), shift([41.001, 2., 1.])]],
                counts: vec![0, 0, 1],
                kinds: vec![0, 0, 0],
                scales: vec![1.; 3],
            };
            assert!(!branch.finite_image_bounds(&unit_domain(1)));
            let cover = cover_feasible(
                |x| branch.evaluate(x).0,
                |x| branch.finite_image_bounds(x),
                unit_domain(1),
                10,
                &work,
            )
            .unwrap();
            assert_eq!(cover.excluded_boxes, 1);
            assert_eq!(cover.unresolved_boxes, 0);
            assert!(cover.roots.is_empty());
            // A real two-bounce path: images intersect at (20,42/31,1),
            // then (0,2/31,1). Do not reject it or a box containing it.
            branch.edges = vec![[shift([9., 2., 1.]), shift([11., 2., 1.])]];
            assert!(branch.finite_image_bounds(&unit_domain(1)));
            assert!(branch.finite_image_bounds(&[I::point(0.5)]));
            // A line lying in the plane has zero numerator and denominator.
            // The homogeneous check alone must retain this uncertain case.
            branch.family = vec![0];
            branch.counts = vec![0, 1];
            branch.kinds = vec![0, 0];
            branch.s = shift([0., 0., 1.]);
            branch.edges = vec![[shift([0., 2., 1.]), shift([0., 3., 1.])]];
            assert!(branch.finite_image_bounds(&unit_domain(1)));
        }
    }

    #[test]
    fn joint_corner_with_two_mirrors_matches_exact_rational_elimination() {
        // Unfold corner (1,-1) through x=0 then x=20 to (41,-1),
        // connect to R=(10,10). Back intersections give B2y=200/31,
        // B1y=-20/31. Corner projection on S=(10,-10)->B1 is 837/901.
        // hC=(1-f)*6+f*hB1 and hB1=(30*hC+1)/31 imply hC=411/91.
        for offset in [[0.; 3], [600000., 300000., 200.]] {
            let shift = |p: Point| add(p, offset);
            let cancel = AtomicBool::new(false);
            let operations = AtomicUsize::new(0);
            let work = Work {
                cancel: &cancel,
                operations: &operations,
            };
            let scene=Scene::prepare(&json!({"reflectors":[
            {"id":"a","vertices":[shift([0.,-100.,0.]),shift([0.,100.,0.]),shift([0.,100.,30.]),shift([0.,-100.,30.])]},
            {"id":"b","vertices":[shift([20.,-100.,0.]),shift([20.,100.,0.]),shift([20.,100.,30.]),shift([20.,-100.,30.])]}
        ]}),&cancel).unwrap();
            let branch = Branch {
                scene: &scene,
                s: shift([10., -10., 6.]),
                r: shift([10., 10., 1.]),
                family: vec![0, 1],
                edges: vec![[shift([1., -1., 0.]), shift([1., -1., 30.])]],
                counts: vec![1, 0, 0],
                kinds: vec![1, 0, 0],
                scales: vec![100. + (290_f64 / 31.).powi(2), 1., 1.],
            };
            let result = cover(|x| branch.evaluate(x).0, unit_domain(1), 2000, &work).unwrap();
            assert_eq!(result.unresolved_boxes, 0, "{result:?}");
            assert_eq!(result.roots.len(), 1, "{result:?}");
            let reduced = Reduced::new(&branch);
            assert!(reduced.free.is_empty());
            let expanded = reduced.parameters(&[]).unwrap();
            let points = branch.root_points(
                &expanded
                    .iter()
                    .map(|v| v.value.midpoint())
                    .collect::<Vec<_>>(),
            );
            let expected = [
                [10., -10., 6.],
                [1., -1., 411. / 91.],
                [0., -20. / 31., 12421. / 2821.],
                [20., 200. / 31., 6021. / 2821.],
                [10., 10., 1.],
            ]
            .map(shift);
            assert_eq!(points.len(), expected.len());
            for (a, b) in points.iter().zip(expected) {
                assert!(distance(*a, b) < 1e-8, "{a:?} {b:?}");
            }
            assert!(specular_direction_matches(
                &scene.faces[0],
                points[1],
                points[2],
                points[3]
            ));
            assert!(specular_direction_matches(
                &scene.faces[1],
                points[2],
                points[3],
                points[4]
            ));
        }
    }

    #[test]
    fn horizontal_roof_identity_survives_unbounded_image_intervals() {
        for offset in [[0.; 3], [600000., 300000., 200.]] {
            let shift = |p: Point| add(p, offset);
            let cancel = AtomicBool::new(false);
            let scene = Scene::prepare(&json!({"reflectors":[
                {"id":"a","vertices":[shift([0.,-10.,0.]),shift([0.,10.,0.]),shift([0.,10.,10.]),shift([0.,-10.,10.])]},
                {"id":"b","vertices":[shift([20.,-10.,0.]),shift([20.,10.,0.]),shift([20.,10.,10.]),shift([20.,-10.,10.])]}
            ]}), &cancel).unwrap();
            let mut branch = Branch {
                scene: &scene,
                s: shift([1., 0., 1.]),
                r: shift([45., 4., 0.]),
                family: vec![0, 1],
                edges: vec![
                    [shift([1., 1., 1.]), shift([2., 2., 1.])],
                    [shift([40., 3., 1.]), shift([43., 3., 1.])],
                ],
                counts: vec![1, 0, 1],
                kinds: vec![1, 0, 0],
                scales: vec![1.; 3],
            };
            // Both vertical reflections preserve z=1 between roof anchors.
            // The second anchor's X interval spans the twice-imaged first
            // anchor's X, so direct quotient intervals are unbounded.
            let domain = unit_domain(2);
            assert!(
                branch
                    .evaluate(&domain)
                    .1
                    .iter()
                    .flatten()
                    .any(|p| !p.value.finite())
            );
            assert_eq!(
                branch.fixed_section_heights(),
                vec![Some(offset[2] + 1.); 3]
                    .into_iter()
                    .chain([Some(offset[2])])
                    .collect::<Vec<_>>()
            );
            assert_eq!(branch.coincident_edges(&domain), vec![0]);
            // A real slope must retain its moving parameter. Exact common
            // height, not a widened tolerance, establishes the identity.
            branch.edges[0][1][2] += 1e-6;
            assert!(branch.coincident_edges(&domain).is_empty());
            branch.edges[0][1][2] -= 1e-6;
            branch.s[2] += 1e-6;
            assert!(branch.coincident_edges(&domain).is_empty());
        }
    }

    #[test]
    fn plane_coincident_roof_edges_use_endpoint_vertices_without_false_absence() {
        for offset in [[0.; 3], [600000., 300000., 200.]] {
            let shift = |p: Point| add(p, offset);
            let cancel = AtomicBool::new(false);
            let operations = AtomicUsize::new(0);
            let work = Work {
                cancel: &cancel,
                operations: &operations,
            };
            let scene=Scene::prepare(&json!({"reflectors":[{"id":"mirror","vertices":[shift([0.,-100.,0.]),shift([0.,100.,0.]),shift([0.,100.,30.]),shift([0.,-100.,30.])]}],"buildings":[
                {"id":"incoming","footprint":[shift([2.,-3.,0.]),shift([4.,-3.,0.]),shift([4.,0.,0.]),shift([2.,0.,0.])],"height_m":4},
                {"id":"outgoing","footprint":[shift([2.,1.,0.]),shift([4.,1.,0.]),shift([4.,3.,0.]),shift([2.,3.,0.])],"height_m":4}
            ]}),&cancel).unwrap();
            let fi = scene
                .faces
                .iter()
                .position(|f| f.object == "mirror")
                .unwrap();
            let mut branch = Branch {
                scene: &scene,
                s: shift([10., -10., 4.]),
                r: shift([10., 10., 1.]),
                family: vec![fi],
                edges: vec![
                    [shift([2., -3., 4.]), shift([2., 0., 4.])],
                    [shift([2., 1., 4.]), shift([2., 3., 4.])],
                ],
                counts: vec![1, 1],
                kinds: vec![1, 0],
                scales: vec![200., 20_f64.sqrt()],
            };
            assert_eq!(branch.coincident_edges(&unit_domain(2)), vec![0]);
            let original = cover(|x| branch.evaluate(x).0, unit_domain(2), 10, &work).unwrap();
            assert!(
                original.unresolved_boxes > 0,
                "a continuous root set is not an isolated root or mathematical absence"
            );
            let canonical = cover_feasible(
                |x| branch.evaluate(x).0,
                |x| branch.coincident_edges(x).is_empty(),
                unit_domain(2),
                10,
                &work,
            )
            .unwrap();
            assert_eq!(canonical.excluded_boxes, 1);
            assert_eq!(canonical.unresolved_boxes, 0);
            let points = branch.root_points(&[0.5, 0.5]);
            let route = Route {
                points,
                reflections: vec![(2, fi)],
                diffraction: vec![1, 3],
                sections: vec![0, 2, 4],
                kinds: vec![1, 0],
                vertical: [0.; 8],
                lateral: [0.; 8],
                terrain_support: false,
            };
            let plan = Plan::from_route(&scene, &route).unwrap();
            let mut queue = std::collections::VecDeque::new();
            let mut seen = HashSet::from([plan.key()]);
            add_corner_endpoints(&scene, &plan, 0, &mut queue, &mut seen, &work).unwrap();
            assert_eq!(queue.len(), 2);
            for next in queue {
                let edge = &scene.contact_supports.edges[next.supports[0]];
                assert!(edge.corner);
                assert_eq!(next.parameters[0], 1.);
                assert!(
                    branch.edges[0]
                        .iter()
                        .any(|p| distance(*p, edge.points[1]) < 1e-8)
                );
            }
            branch.edges[1] = [shift([2., 1., 3.]), shift([2., 3., 3.])];
            assert!(
                branch.coincident_edges(&unit_domain(2)).is_empty(),
                "an actual sloping section still has sliding roof intersections"
            );
        }
    }

    #[test]
    fn affine_elimination_preserves_independent_moving_edge_and_corner_contacts() {
        // Extend the independently authored four-edge case with corners in its
        // lateral section plane. At (3,-3) and (1,-1), the affine heights are
        // 17/4 and 15/4. They must be eliminated without fixing a trial height.
        for offset in [[0.; 3], [600000., 300000., 200.]] {
            let shift = |p: Point| add(p, offset);
            let cancel = AtomicBool::new(false);
            let scene=Scene::prepare(&json!({"reflectors":[{"id":"wall","vertices":[shift([0.,-100.,0.]),shift([0.,100.,0.]),shift([0.,100.,30.]),shift([0.,-100.,30.])]}]}),&cancel).unwrap();
            let branch = Branch {
                scene: &scene,
                s: shift([10., -10., 6.]),
                r: shift([10., 10., 1.]),
                family: vec![0],
                edges: vec![
                    [[4., -4., 37. / 8.], [6., -4., 37. / 8.]],
                    [[3., -3., 0.], [3., -3., 10.]],
                    [[2., -3., 4.], [2., 0., 4.]],
                    [[1., -1., 0.], [1., -1., 10.]],
                    [[2., 1., 3.], [2., 3., 3.]],
                    [[6., 5., 2.], [6., 7., 2.]],
                ]
                .into_iter()
                .map(|e| e.map(shift))
                .collect(),
                counts: vec![4, 2],
                kinds: vec![1, 0],
                scales: vec![200., 20_f64.sqrt()],
            };
            let reduced = Reduced::new(&branch);
            assert_eq!(reduced.linear, vec![1, 3]);
            assert_eq!(reduced.free, vec![0, 2, 4, 5]);
            let parameters = [0.5, 1. / 3., 0.5, 0.5];
            let neighborhood: Vec<_> = parameters
                .iter()
                .map(|p| I {
                    lo: p - 0.001,
                    hi: p + 0.001,
                })
                .collect();
            let proof = krawczyk(&|x| reduced.evaluate(x), &neighborhood).unwrap();
            assert!(
                proof
                    .iter()
                    .zip(&neighborhood)
                    .all(|(a, b)| a.lo > b.lo && a.hi < b.hi)
            );
            let expanded = reduced.parameters(&parameters.map(I::point)).unwrap();
            let points = branch.root_points(
                &expanded
                    .iter()
                    .map(|v| v.value.midpoint())
                    .collect::<Vec<_>>(),
            );
            let expected = [
                [10., -10., 6.],
                [5., -4., 37. / 8.],
                [3., -3., 17. / 4.],
                [2., -2., 4.],
                [1., -1., 15. / 4.],
                [0., 0., 7. / 2.],
                [2., 2., 3.],
                [6., 6., 2.],
                [10., 10., 1.],
            ]
            .map(shift);
            assert_eq!(points.len(), expected.len());
            for (p, e) in points.iter().zip(expected) {
                assert!(distance(*p, e) < 1e-8, "{p:?} {e:?}");
            }
        }
    }

    #[test]
    fn four_moving_edges_match_independent_plane_and_image_equations() {
        // Authored simultaneous case, not an ISO reference scene. P and H1 have
        // heights 4 and 3, so their x=0 mirror has height 7/2. Equal distances
        // xP=xH1=2 give Cy=(Py+H1y)/2. Vertical outgoing incidence gives
        // H1y=4/5*Cy+2 and Py=6/5*Cy-2. P's lateral-plane equation reduces to
        // (2/5)*Cy*(Cy+10)=0. The finite P/H1 edges exclude Cy=-10.
        // Hence C=(0,0,7/2), P=(2,-2,4), H1=(2,2,3), H2=(6,6,2).
        // E's plane fraction is 11/20, fixing E=(5,-4,37/8).
        for offset in [[0.; 3], [600000., 300000., 200.]] {
            let shift = |p: Point| add(p, offset);
            let cancel = AtomicBool::new(false);
            let operations = AtomicUsize::new(0);
            let work = Work {
                cancel: &cancel,
                operations: &operations,
            };
            let scene=Scene::prepare(&json!({"reflectors":[{"id":"wall","vertices":[shift([0.,-100.,0.]),shift([0.,100.,0.]),shift([0.,100.,30.]),shift([0.,-100.,30.])]}]}),&cancel).unwrap();
            let edges = [
                [[4., -4., 37. / 8.], [6., -4., 37. / 8.]],
                [[2., -3., 4.], [2., 0., 4.]],
                [[2., 1., 3.], [2., 3., 3.]],
                [[6., 5., 2.], [6., 7., 2.]],
            ]
            .map(|e| e.map(shift))
            .to_vec();
            let branch = Branch {
                scene: &scene,
                s: shift([10., -10., 6.]),
                r: shift([10., 10., 1.]),
                family: vec![0],
                edges,
                counts: vec![2, 2],
                kinds: vec![1, 0],
                scales: vec![200., 20_f64.sqrt()],
            };
            let mut result = cover(|x| branch.evaluate(x).0, unit_domain(4), 2000, &work).unwrap();
            assert_eq!(result.unresolved_boxes, 0, "{result:?}");
            assert_eq!(result.roots.len(), 1, "{result:?}");
            let widths: Vec<_> = branch
                .edges
                .iter()
                .map(|e| CONTACT_ENCLOSURE_TOLERANCE_M / distance(e[0], e[1]))
                .collect();
            assert!(
                refine_root(
                    &|x| branch.evaluate(x).0,
                    &mut result.roots[0],
                    |root| root
                        .enclosure
                        .iter()
                        .zip(&widths)
                        .all(|(b, w)| b.hi - b.lo <= *w),
                    20,
                    &work
                )
                .unwrap()
            );
            let points = branch.root_points(&result.roots[0].parameters);
            let expected = [
                [10., -10., 6.],
                [5., -4., 37. / 8.],
                [2., -2., 4.],
                [0., 0., 7. / 2.],
                [2., 2., 3.],
                [6., 6., 2.],
                [10., 10., 1.],
            ]
            .map(shift);
            assert_eq!(points.len(), expected.len());
            for (a, b) in points.iter().zip(expected) {
                assert!(distance(*a, b) < 1e-6, "{a:?} {b:?}");
            }
            for (bound, exact) in result.roots[0]
                .enclosure
                .iter()
                .zip([0.5, 1. / 3., 0.5, 0.5])
            {
                assert!(bound.lo <= exact && exact <= bound.hi, "{bound:?} {exact}");
            }
            assert!(specular_direction_matches(
                &scene.faces[0],
                points[2],
                points[3],
                points[4]
            ));
        }
    }

    #[test]
    fn vertical_anchor_lines_bound_each_section_without_crossing_reflections() {
        // S=(0,0,1) reflects on x=5 at (5,5,1) and reaches R=(0,10,1).
        // Anchors (1,1),(2,2) and (2,8),(1,9) lie on its two plan lines.
        // Each line is necessary independently of the image construction.
        for offset in [[0.; 3], [600000., 300000., 200.]] {
            let shift = |p: Point| add(p, offset);
            let cancel = AtomicBool::new(false);
            let scene = Scene::prepare(&json!({"reflectors":[
                {"id":"mirror","vertices":[shift([5.,-10.,0.]),shift([5.,20.,0.]),shift([5.,20.,10.]),shift([5.,-10.,10.])]}
            ]}), &cancel).unwrap();
            let mut branch = Branch {
                scene: &scene,
                s: shift([0., 0., 1.]),
                r: shift([0., 10., 1.]),
                family: vec![0],
                edges: vec![
                    [shift([1., 0., 1.]), shift([1., 2., 1.])],
                    [shift([2., 1., 1.]), shift([2., 3., 1.])],
                    [shift([2., 7., 1.]), shift([2., 9., 1.])],
                    [shift([1., 8., 1.]), shift([1., 10., 1.])],
                ],
                counts: vec![2, 2],
                kinds: vec![0, 0],
                scales: vec![1., 1.],
            };
            let valid = vec![I::point(0.5); 4];
            assert!(branch.vertical_anchor_bounds(&valid));
            assert!(branch.feasible(&valid));
            assert!(branch.vertical_anchor_bounds(&unit_domain(4)));
            let mut invalid = valid.clone();
            invalid[0] = I { lo: 0.7, hi: 0.8 };
            assert!(!branch.vertical_anchor_bounds(&invalid));
            assert!(!branch.feasible(&invalid));
            invalid = valid.clone();
            invalid[3] = I { lo: 0.7, hi: 0.8 };
            assert!(!branch.vertical_anchor_bounds(&invalid));
            // Lateral sections may bend in plan; this predicate must not
            // apply their vertical-section collinearity requirement.
            branch.kinds[1] = 1;
            assert!(branch.vertical_anchor_bounds(&invalid));
        }
    }
    #[test]
    fn unfolded_constraints_contract_through_multiple_mirrors_and_empty_sections() {
        // Exact path: (1,0)->(0,1)->(20,21)->(1,40). Anchors at
        // (1/2,1/2) and (10,31) straddle both mirrors. This independently
        // exercises forward and reverse unfolding order and an empty section.
        for offset in [[0.; 3], [600000., 300000., 200.]] {
            let shift = |p: Point| add(p, offset);
            let cancel = AtomicBool::new(false);
            let scene = Scene::prepare(&json!({"reflectors":[
                {"id":"first","vertices":[shift([0.,-10.,0.]),shift([0.,50.,0.]),shift([0.,50.,10.]),shift([0.,-10.,10.])]},
                {"id":"second","vertices":[shift([20.,-10.,0.]),shift([20.,50.,0.]),shift([20.,50.,10.]),shift([20.,-10.,10.])]}
            ]}), &cancel).unwrap();
            let mut branch = Branch {
                scene: &scene,
                s: shift([1., 0., 1.]),
                r: shift([1., 40., 1.]),
                family: vec![0, 1],
                edges: vec![
                    [shift([0.5, 0., 1.]), shift([0.5, 1., 1.])],
                    [shift([10., 30., 1.]), shift([10., 32., 1.])],
                ],
                counts: vec![1, 0, 1],
                kinds: vec![0, 0, 0],
                scales: vec![1.; 3],
            };
            let full = unit_domain(2);
            let reduced = Reduced::new(&branch);
            let constraints = |x: &[I]| reduced.constraints(x, &full);
            let mut domain = full.clone();
            assert!(contract_constraints(&constraints, &mut domain));
            for b in &domain {
                assert!(b.lo <= 0.5 && b.hi >= 0.5, "{b:?}");
                assert!(b.hi - b.lo < 1e-4, "{b:?}");
            }
            let root = [I::point(0.5); 2];
            for v in branch
                .evaluate(&root)
                .0
                .into_iter()
                .chain(constraints(&root))
            {
                assert!(v.value.lo <= 0. && v.value.hi >= 0., "{:?}", v.value);
            }
            // A lateral first section supplies its section-plane equation, not
            // plan collinearity. Its start is the source (dA = 1), so the
            // polynomial is the image-chain residual times dB^2 > 0: same sign.
            // A sloped edge keeps the residual nonzero (a flat scene is all zeros).
            branch.kinds[0] = 1;
            branch.edges[0] = [shift([0.5, 0., 2.]), shift([0.5, 1., 4.])];
            let reduced = Reduced::new(&branch);
            assert_eq!(reduced.constraints(&full, &full).len(), 2);
            let mut compared = 0;
            for i in 0..=8 {
                for j in 0..=8 {
                    let x = [I::point(i as f64 / 8.), I::point(j as f64 / 8.)];
                    let residual = branch.evaluate(&x).0[0].value.midpoint();
                    let polynomial = reduced.constraints(&x, &full)[1].value.midpoint();
                    if residual.abs() > 1e-9 {
                        assert_eq!(residual.signum(), polynomial.signum(), "{x:?}");
                        compared += 1;
                    }
                }
            }
            assert!(compared > 40);
        }
    }

    #[test]
    fn lateral_section_at_roof_height_is_excluded_without_image_poles() {
        // Engineering regression geometry, not an ISO case: a surface
        // source on a flat roof at roof height, a lateral first section over
        // two roof edges in that plane and three wall mirrors, all at
        // 216.1032 m. Points show the lateral residual -Dz*(V.Dxy) > 0, so no
        // root exists. Over boxes, the nested image quotients for the section
        // end are unbounded and exhausted the interval budget.
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        let wall = |id: &str, a: [f64; 2], b: [f64; 2]| {
            json!({"id": id, "side": "front", "vertices": [
                [a[0], a[1], 216.1032], [b[0], b[1], 216.1032],
                [b[0], b[1], 211.836], [a[0], a[1], 211.836]]})
        };
        let scene = Scene::prepare(&json!({"reflectors": [
            wall("55", [600874.5149026552, 300444.36760046496], [600862.3316231697, 300427.9782406327]),
            wall("53", [600867.2168838244, 300424.3467079478], [600879.4001633099, 300440.73606778]),
            wall("32", [600706.1661993122, 300341.502293352], [600706.020952932, 300328.5490817534]),
        ]}), &cancel)
        .unwrap();
        let corner = [600901.1108242723, 300469.94996650016, 216.1032];
        let branch = Branch {
            scene: &scene,
            s: [600902.4056600265, 300460.78394874814, 216.1032],
            r: [600800.168900514, 300271.8010176537, 212.01097271855755],
            family: vec![0, 1, 2],
            edges: vec![
                [[600899.3237805015, 300445.8324069943, 216.1032], corner],
                [[600888.927544787, 300453.56060666795, 216.1032], corner],
            ],
            counts: vec![2, 0, 0, 0],
            kinds: vec![1, 0, 0, 0],
            scales: vec![1.; 4],
        };
        let reduced = Reduced::new(&branch);
        let domain = unit_domain(2);
        let x = [I::point(0.5), I::point(0.5)];
        assert!(branch.evaluate(&x).0.iter().all(|v| v.value.lo > 0.));
        let cover = |constrained: bool, budget: usize| {
            cover_constrained(
                |x| reduced.evaluate(x),
                |x| reduced.feasible(x, &domain),
                |x| {
                    if constrained {
                        reduced.constraints(x, &domain)
                    } else {
                        vec![]
                    }
                },
                domain.clone(),
                budget,
                &work,
            )
            .unwrap()
        };
        // Either pole-free mechanism settles it on its own: the lateral
        // constraint, or the direct (unchained) contact construction. The
        // former chained quotients exhausted the budget without the constraint.
        for constrained in [true, false] {
            let proved = cover(constrained, ROOT_BOX_BUDGET);
            assert_eq!((proved.roots.len(), proved.unresolved_boxes), (0, 0));
        }
    }

    #[test]
    fn necessary_constraint_contraction_preserves_multiple_roots_and_continua() {
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        let polynomial = |x: &[I]| {
            let a = Dual::variable(x[0], 2, 0);
            let b = Dual::variable(x[1], 2, 1);
            vec![
                a.clone() * a - Dual::constant(0.25, 2),
                b - Dual::constant(0.25, 2),
            ]
        };
        let domain = vec![I { lo: -1., hi: 1. }; 2];
        let mut contracted = domain.clone();
        assert!(contract_constraints(&polynomial, &mut contracted));
        assert!(contracted[0].lo <= -0.5 && contracted[0].hi >= 0.5);
        assert!(contracted[1].lo <= 0.25 && contracted[1].hi >= 0.25);
        let roots =
            cover_constrained(polynomial, |_| true, polynomial, domain, 128, &work).unwrap();
        assert_eq!(roots.unresolved_boxes, 0, "{roots:?}");
        assert_eq!(roots.roots.len(), 2, "{roots:?}");
        for exact in [-0.5, 0.5] {
            assert!(
                roots
                    .roots
                    .iter()
                    .any(|r| r.enclosure[0].lo <= exact && exact <= r.enclosure[0].hi)
            );
        }
        let continuum = |x: &[I]| {
            vec![Dual::variable(x[0], 2, 0) + Dual::variable(x[1], 2, 1) - Dual::constant(1., 2)]
        };
        let mut domain = unit_domain(2);
        assert!(contract_constraints(&continuum, &mut domain));
        assert!(domain.iter().all(|b| b.lo <= 0. && b.hi >= 1.));
        // Necessary constraints are never sufficient evidence of a root.
        let uncertain = cover_constrained(
            |x| {
                let mut rows = continuum(x);
                rows.push(Dual::constant(0., 2));
                rows
            },
            |_| true,
            continuum,
            unit_domain(2),
            16,
            &work,
        )
        .unwrap();
        assert!(uncertain.unresolved_boxes > 0);
        assert!(uncertain.roots.is_empty());
    }

    #[test]
    fn necessary_constraints_prove_absence_without_a_nonsingular_system() {
        let constraints = |x: &[I]| {
            let v = Dual::variable(x[0], 1, 0);
            vec![v.clone() - v + Dual::constant(0.25, 1)]
        };
        let mut domain = unit_domain(1);
        assert!(!contract_constraints(&constraints, &mut domain));
    }

    #[test]
    fn reverse_unfolding_uses_the_actual_inverse_of_prepared_mirror_coefficients() {
        // Amplify normal rounding to expose an invalid involution assumption.
        // With n=(3/2,0,0), the implemented image sends x to -7x/2.
        // For S=(2,0), P=(1,2t), R=(2,4), the original image intersection
        // and section equation give the independent exact t=7/9.
        let cancel = AtomicBool::new(false);
        let mut scene = Scene::prepare(
            &json!({"reflectors":[{"id":"mirror","vertices":
            [[0.,-10.,0.],[0.,10.,0.],[0.,10.,10.],[0.,-10.,10.]]}]}),
            &cancel,
        )
        .unwrap();
        scene.faces[0].normal = [1.5, 0., 0.];
        let branch = Branch {
            scene: &scene,
            s: [2., 0., 1.],
            r: [2., 4., 1.],
            family: vec![0],
            edges: vec![[[1., 0., 1.], [1., 2., 1.]]],
            counts: vec![1, 0],
            kinds: vec![0, 0],
            scales: vec![1., 1.],
        };
        let full = unit_domain(1);
        let reduced = Reduced::new(&branch);
        let mut domain = full.clone();
        assert!(contract_constraints(
            &|x| reduced.constraints(x, &full),
            &mut domain
        ));
        assert!(
            domain[0].lo <= 7. / 9. && domain[0].hi >= 7. / 9.,
            "{:?}",
            domain[0]
        );
        assert!(domain[0].hi - domain[0].lo < 1e-12);
        let original = branch.evaluate(&domain).0;
        assert!(original[0].value.lo <= 0. && original[0].value.hi >= 0.);
    }
}
