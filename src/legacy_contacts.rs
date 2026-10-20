//! Historical contact solvers retained only as supplementary test oracles.
use super::*;
/// Solve vertical-mirror elevations for fixed plan-view supports. Each lateral
/// support is affine in its section endpoint heights; roof supports are fixed.
/// This is only a proposal: the next iteration rebuilds and validates the route.
fn mixed_contact_heights(scene: &Scene, route: &Route) -> Option<Vec<f64>> {
    let n = route.reflections.len();
    if n == 0 || route.sections.len() != n + 2 {
        return None;
    }
    let contacts: Vec<_> = route.sections.iter().map(|i| route.points[*i]).collect();
    let origin = contacts[0][2];
    let mut matrix = vec![vec![0.; n + 1]; n];
    for (i, (at, fi)) in route.reflections.iter().enumerate() {
        let face = &scene.faces[*fi];
        if !face.vertical()
            || *at != route.sections[i + 1]
            || *at == 0
            || *at + 1 >= route.points.len()
        {
            return None;
        }
        let dl = face.signed(route.points[at - 1]);
        let dr = face.signed(route.points[at + 1]);
        if dl * dr <= 0. || (dl + dr).abs() < 1e-12 {
            return None;
        }
        let weight = dr / (dl + dr);
        let row = &mut matrix[i];
        row[i] = 1.;
        for (section, support, w) in [(i, at - 1, weight), (i + 1, at + 1, 1. - weight)] {
            let mut endpoint = |index: usize, coefficient: f64| {
                if index == 0 || index == n + 1 {
                    row[n] += coefficient * (contacts[index][2] - origin);
                } else {
                    row[index - 1] -= coefficient;
                }
            };
            if support == route.sections[section] {
                endpoint(section, w);
            } else if support == route.sections[section + 1] {
                endpoint(section + 1, w);
            } else if route.kinds[section] != 0 {
                let a = contacts[section];
                let b = contacts[section + 1];
                let p = route.points[support];
                let dx = b[0] - a[0];
                let dy = b[1] - a[1];
                let fraction = ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / (dx * dx + dy * dy);
                endpoint(section, w * (1. - fraction));
                endpoint(section + 1, w * fraction);
            } else {
                row[n] += w * (route.points[support][2] - origin);
            }
        }
        if row.iter().any(|v| !v.is_finite()) {
            return None;
        }
    }
    // Partial pivoting; singular/ill-conditioned systems retain ordinary search.
    // Failure to accelerate must never be interpreted as a missing path.
    for col in 0..n {
        let pivot =
            (col..n).max_by(|a, b| matrix[*a][col].abs().total_cmp(&matrix[*b][col].abs()))?;
        if matrix[pivot][col].abs() < 1e-12 {
            return None;
        }
        matrix.swap(col, pivot);
        let divisor = matrix[col][col];
        for k in col..=n {
            matrix[col][k] /= divisor;
        }
        for row in col + 1..n {
            let factor = matrix[row][col];
            for k in col..=n {
                matrix[row][k] -= factor * matrix[col][k];
            }
        }
    }
    let mut heights = vec![0.; n];
    for i in (0..n).rev() {
        heights[i] = matrix[i][n];
        for j in i + 1..n {
            heights[i] -= matrix[i][j] * heights[j];
        }
        if !heights[i].is_finite() {
            return None;
        }
    }
    for z in &mut heights {
        *z += origin;
    }
    heights.iter().all(|z| z.is_finite()).then_some(heights)
}

fn updated_selected_heights(
    route: &Route,
    heights: &[f64],
    selected: &[Vec<Point>],
) -> Option<Vec<Vec<Point>>> {
    let mut selected = selected.to_vec();
    let mut contacts: Vec<_> = route.sections.iter().map(|i| route.points[*i]).collect();
    for (point, z) in contacts.iter_mut().skip(1).zip(heights) {
        point[2] = *z;
    }
    for (j, points) in selected.iter_mut().enumerate() {
        let a = contacts[j];
        let b = contacts[j + 1];
        let last = points.len() - 1;
        points[0] = a;
        points[last] = b;
        if route.kinds[j] != 0 {
            let dx = b[0] - a[0];
            let dy = b[1] - a[1];
            for p in &mut points[1..last] {
                let fraction = ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / (dx * dx + dy * dy);
                p[2] = a[2] + fraction * (b[2] - a[2]);
            }
        }
    }
    selected
        .iter()
        .flatten()
        .flatten()
        .all(|v| v.is_finite())
        .then_some(selected)
}

/// Solve a discovered support topology jointly. Vertical roof/terrain anchors
/// remain fixed; lateral chains are recut on their physical supports. A proposal
/// is accepted only if the complete rebuilt route preserves those constraints.
/// Singular equations, changed topology or failed validation retain iteration.
fn fixed_support_height_route(
    scene: &Scene,
    s: Point,
    r: Point,
    family: &Family,
    route: &Route,
    band: usize,
    vertical_offset: f64,
    work: &Work,
) -> Result<Option<Route>, Diagnostic> {
    if route
        .reflections
        .iter()
        .any(|(at, _)| route.diffraction.contains(at))
    {
        return Ok(None);
    }
    let Some(heights) = mixed_contact_heights(scene, route) else {
        return Ok(None);
    };
    let choices: Vec<_> = route
        .sections
        .windows(2)
        .map(|w| route.points[w[0]..=w[1]].to_vec())
        .collect();
    let Some(mut updated) = updated_selected_heights(route, &heights, &choices) else {
        return Ok(None);
    };
    for (j, points) in updated.iter_mut().enumerate() {
        work.tick()?;
        if route.kinds[j] == 0 {
            // Mandatory vertical anchors must still form the controlling upper
            // hull at the proposed contact heights, not a downward detour.
            let trace = cumulative(points);
            let hull = upper_hull(
                points
                    .iter()
                    .zip(&trace)
                    .map(|(p, t)| Support {
                        s: *t,
                        z: p[2],
                        terrain: false,
                    })
                    .collect(),
            );
            if hull.len() != points.len() {
                return Ok(None);
            }
        } else {
            let mut candidate = None;
            for v in crate::lateral::candidates_on_supports(
                scene,
                points[0],
                *points.last().unwrap(),
                band,
                vertical_offset,
                &choices[j],
                work,
            )? {
                if same_lateral_chain(scene, &choices[j], &v, work)? {
                    candidate = Some(v);
                    break;
                }
            }
            let Some(candidate) = candidate else {
                return Ok(None);
            };
            *points = candidate;
        }
    }
    let proposed_contacts: Vec<_> = std::iter::once(updated[0][0])
        .chain(updated.iter().map(|v| *v.last().unwrap()))
        .collect();
    let Some((points, reflections, sections)) = resolve_choices(scene, s, r, family, &updated)?
    else {
        return Ok(None);
    };
    let Some(result) = build_route(scene, &points, &reflections, &sections, &route.kinds, band)?
    else {
        return Ok(None);
    };
    if result.points.len() != updated.iter().map(Vec::len).sum::<usize>() - updated.len() + 1
        || result.sections.len() != proposed_contacts.len()
        || result
            .sections
            .iter()
            .zip(&proposed_contacts)
            .any(|(at, p)| distance(result.points[*at], *p) > 1e-7)
        || !same_contact_supports(scene, route, &result, work)?
        || !result.reflections.iter().all(|(at, fi)| {
            specular_direction_matches(
                &scene.faces[*fi],
                result.points[at - 1],
                result.points[*at],
                result.points[at + 1],
            )
        })
    {
        return Ok(None);
    }
    Ok(Some(result))
}

/// The height equations use section endpoints and the two neighbours of each
/// mirror. Other lateral vertices can slide along roof edges as heights change;
/// waiting for those vertices would prevent the coupled solve from starting.
fn height_support_plan_settled(previous: &Route, current: &Route) -> bool {
    previous.points.len() == current.points.len()
        && previous.sections == current.sections
        && previous.reflections == current.reflections
        && previous.kinds == current.kinds
        && current
            .sections
            .iter()
            .copied()
            .chain(
                current
                    .reflections
                    .iter()
                    .flat_map(|(at, _)| [at - 1, *at, at + 1]),
            )
            .all(|i| horizontal(previous.points[i], current.points[i]) <= 1e-8)
}

/// Match physical supports, allowing a lateral vertex to slide on the same
/// finite roof edge. A corner/edge change is not a settled search branch.
fn same_contact_supports(
    scene: &Scene,
    a: &Route,
    b: &Route,
    work: &Work,
) -> Result<bool, Diagnostic> {
    if a.points.len() != b.points.len()
        || a.sections != b.sections
        || a.reflections != b.reflections
        || a.kinds != b.kinds
        || a.diffraction != b.diffraction
    {
        return Ok(false);
    }
    for i in 1..a.points.len() - 1 {
        if a.reflections.iter().any(|(at, _)| *at == i)
            || horizontal(a.points[i], b.points[i]) <= 1e-8
        {
            continue;
        }
        let mut shared_edge = false;
        for bi in scene.building_index.candidates(a.points[i], b.points[i]) {
            work.tick()?;
            let building = &scene.buildings[bi];
            for triangle in &building.roof_triangles {
                for k in 0..3 {
                    let p = building.roof[triangle[k]];
                    let q = building.roof[triangle[(k + 1) % 3]];
                    if point_segment_distance(a.points[i], p, q) <= 1e-8
                        && point_segment_distance(b.points[i], p, q) <= 1e-8
                    {
                        shared_edge = true;
                    }
                }
            }
        }
        if !shared_edge {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Vector delta-squared extrapolation of mirror contacts. It is a proposal,
/// never a convergence or infeasibility decision. Recut and validate its branch
/// and require a smaller contact residual than the ordinary next update.
fn extrapolated_contacts(older: &Route, previous: &Route, current: &Route) -> Option<Vec<Point>> {
    if older.reflections != current.reflections || previous.reflections != current.reflections {
        return None;
    }
    let mut numerator = 0.;
    let mut denominator = 0.;
    for (at, _) in &current.reflections {
        let delta = sub(current.points[*at], previous.points[*at]);
        let change = sub(sub(previous.points[*at], older.points[*at]), delta);
        numerator += dot(delta, change);
        denominator += dot(change, change);
    }
    if denominator == 0. || !denominator.is_finite() {
        return None;
    }
    let factor = numerator / denominator;
    let mut contacts: Vec<_> = current
        .sections
        .iter()
        .map(|i| current.points[*i])
        .collect();
    for (i, (at, _)) in current.reflections.iter().enumerate() {
        contacts[i + 1] = add(
            current.points[*at],
            scale(sub(current.points[*at], previous.points[*at]), factor),
        );
    }
    contacts
        .iter()
        .flatten()
        .all(|v| v.is_finite())
        .then_some(contacts)
}

fn accelerated_contact_choices(
    scene: &Scene,
    s: Point,
    r: Point,
    family: &Family,
    older: &Route,
    previous: &Route,
    current: &Route,
    selected: &[Vec<Point>],
    band: usize,
    vertical_offset: f64,
    work: &Work,
) -> Result<Option<Vec<Vec<Point>>>, Diagnostic> {
    if !same_contact_supports(scene, older, previous, work)?
        || !same_contact_supports(scene, previous, current, work)?
    {
        return Ok(None);
    }
    let Some(contacts) = extrapolated_contacts(older, previous, current) else {
        return Ok(None);
    };
    if current
        .reflections
        .iter()
        .enumerate()
        .any(|(i, (_, fi))| !scene.faces[*fi].contains(contacts[i + 1]))
    {
        return Ok(None);
    }
    let mut trial = Vec::with_capacity(selected.len());
    for j in 0..selected.len() {
        work.tick()?;
        if current.kinds[j] == 0 {
            trial.push(vec![contacts[j], contacts[j + 1]]);
        } else {
            let side = lateral_side(&selected[j]);
            let candidate = crate::lateral::candidates(
                scene,
                contacts[j],
                contacts[j + 1],
                band,
                vertical_offset,
                work,
            )?
            .into_iter()
            .find(|p| p.len() > 2 && lateral_side(p) == side);
            let Some(candidate) = candidate else {
                return Ok(None);
            };
            trial.push(candidate);
        }
    }
    let rebuild = |choices: &[Vec<Point>]| -> Result<Option<Route>, Diagnostic> {
        let Some((points, reflections, sections)) = resolve_choices(scene, s, r, family, choices)?
        else {
            return Ok(None);
        };
        build_route(
            scene,
            &points,
            &reflections,
            &sections,
            &current.kinds,
            band,
        )
    };
    let Some(proposed) = rebuild(&trial)? else {
        return Ok(None);
    };
    let Some(ordinary) = rebuild(selected)? else {
        return Ok(None);
    };
    if !same_contact_supports(scene, current, &proposed, work)? {
        return Ok(None);
    }
    let residual = |route: &Route, endpoints: &[Point]| -> f64 {
        route
            .reflections
            .iter()
            .enumerate()
            .map(|(i, (at, _))| {
                let delta = sub(route.points[*at], endpoints[i + 1]);
                dot(delta, delta)
            })
            .sum()
    };
    let current_contacts: Vec<_> = current
        .sections
        .iter()
        .map(|i| current.points[*i])
        .collect();
    if residual(&proposed, &contacts) >= residual(&ordinary, &current_contacts) {
        return Ok(None);
    }
    Ok(Some(trial))
}

// Outward-rounded intervals certify a monotone finite-edge equation before
// treating an inconsistent contact branch as absent. Uncertain cases retain
// the ordinary solver and its numerical failure.
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
impl std::ops::Div for ContactInterval {
    type Output = Self;
    fn div(self, b: Self) -> Self {
        if b.lo <= 0. && b.hi >= 0. {
            return Self {
                lo: f64::NEG_INFINITY,
                hi: f64::INFINITY,
            };
        }
        self * Self {
            lo: (1. / b.hi).next_down(),
            hi: (1. / b.lo).next_up(),
        }
    }
}
#[derive(Clone, Copy)]
struct ContactDual {
    value: ContactInterval,
    derivative: ContactInterval,
}
impl ContactDual {
    fn constant(v: f64) -> Self {
        Self {
            value: ContactInterval::point(v),
            derivative: ContactInterval::point(0.),
        }
    }
    fn variable(value: ContactInterval) -> Self {
        Self {
            value,
            derivative: ContactInterval::point(1.),
        }
    }
}
impl std::ops::Add for ContactDual {
    type Output = Self;
    fn add(self, b: Self) -> Self {
        Self {
            value: self.value + b.value,
            derivative: self.derivative + b.derivative,
        }
    }
}
impl std::ops::Sub for ContactDual {
    type Output = Self;
    fn sub(self, b: Self) -> Self {
        Self {
            value: self.value - b.value,
            derivative: self.derivative - b.derivative,
        }
    }
}
impl std::ops::Mul for ContactDual {
    type Output = Self;
    fn mul(self, b: Self) -> Self {
        Self {
            value: self.value * b.value,
            derivative: self.derivative * b.value + self.value * b.derivative,
        }
    }
}
impl std::ops::Div for ContactDual {
    type Output = Self;
    fn div(self, b: Self) -> Self {
        Self {
            value: self.value / b.value,
            derivative: (self.derivative * b.value - self.value * b.derivative)
                / (b.value * b.value),
        }
    }
}
struct RoofContactEquation {
    source: Point,
    receiver: Point,
    origin: Point,
    normal: Point,
    host_edge: [Point; 2],
    screen_edge: Option<[Point; 2]>,
    lateral_corner: Option<Point>,
}
struct RoofContactEvaluation {
    residual: ContactDual,
    contact: [ContactDual; 3],
    support: [ContactDual; 3],
    screen_fraction: Option<ContactDual>,
    screen_parameter: Option<ContactDual>,
}
impl RoofContactEquation {
    fn evaluate(&self, range: ContactInterval) -> Option<RoofContactEvaluation> {
        let c = ContactDual::constant;
        let t = ContactDual::variable(range);
        // Source-relative coordinates keep interval bounds useful at GIS scale.
        let p: [_; 3] = std::array::from_fn(|k| {
            c(self.host_edge[0][k]) - c(self.source[k])
                + t * (c(self.host_edge[1][k]) - c(self.host_edge[0][k]))
        });
        let r = self.receiver.map(c);
        let r: [_; 3] = std::array::from_fn(|k| r[k] - c(self.source[k]));
        let origin: [_; 3] = std::array::from_fn(|k| c(self.origin[k]) - c(self.source[k]));
        let signed = |point: [ContactDual; 3]| {
            (point[0] - origin[0]) * c(self.normal[0]) + (point[1] - origin[1]) * c(self.normal[1])
        };
        let endpoint = self.lateral_corner.unwrap_or(self.receiver);
        let endpoint: [_; 3] = std::array::from_fn(|k| c(endpoint[k]) - c(self.source[k]));
        let dp = signed(p);
        let dr = signed(endpoint);
        if !((dp.value.lo > 0. && dr.value.lo > 0.) || (dp.value.hi < 0. && dr.value.hi < 0.))
            || !dp.value.finite()
            || !dr.value.finite()
        {
            return None;
        }
        let image: [_; 3] = std::array::from_fn(|k| p[k] - c(2. * self.normal[k]) * dp);
        let fraction = dp / (dp + dr);
        let mut contact: [_; 3] =
            std::array::from_fn(|k| image[k] + fraction * (endpoint[k] - image[k]));
        if self.lateral_corner.is_some() {
            if self.screen_edge.is_some() {
                return None;
            }
            // The fixed corner belongs to the outgoing lateral plane. Its
            // elevation is affine in C and R, not a fixed roof elevation.
            let v: [_; 3] = std::array::from_fn(|k| r[k] - contact[k]);
            let q: [_; 3] = std::array::from_fn(|k| endpoint[k] - contact[k]);
            let g = (q[0] * v[0] + q[1] * v[1]) / (v[0] * v[0] + v[1] * v[1]);
            contact[2] = ((c(1.) - fraction) * p[2] + fraction * g * r[2])
                / (c(1.) - fraction * (c(1.) - g));
        }
        let mut screen_fraction = None;
        let mut screen_parameter = None;
        if let Some([a, b]) = self.screen_edge {
            let a: [_; 3] = std::array::from_fn(|k| c(a[k]) - c(self.source[k]));
            let edge: [_; 3] = std::array::from_fn(|k| c(b[k]) - c(self.source[k]) - a[k]);
            let v: [_; 3] = std::array::from_fn(|k| r[k] - contact[k]);
            let q: [_; 3] = std::array::from_fn(|k| a[k] - contact[k]);
            let cross = |a: [ContactDual; 3], b: [ContactDual; 3]| a[0] * b[1] - a[1] * b[0];
            let denominator = cross(v, edge);
            let u = cross(q, edge) / denominator;
            let parameter = cross(q, v) / denominator;
            let screen: [_; 3] = std::array::from_fn(|k| contact[k] + u * v[k]);
            let dq = signed(screen);
            contact[2] = (dq * p[2] + dp * a[2]) / (dp + dq);
            screen_fraction = Some(u);
            screen_parameter = Some(parameter);
        }
        let residual = contact[2] * (p[0] * contact[0] + p[1] * contact[1])
            - p[2] * (contact[0] * contact[0] + contact[1] * contact[1]);
        if !residual.value.finite()
            || !residual.derivative.finite()
            || contact.iter().any(|p| !p.value.finite())
        {
            return None;
        }
        Some(RoofContactEvaluation {
            residual,
            contact,
            support: p,
            screen_fraction,
            screen_parameter,
        })
    }
    /// None means uncertified. Some(None) proves no root on this finite edge;
    /// Some(Some(range)) encloses its unique root by a monotone derivative bound.
    fn root(&self, work: &Work) -> Result<Option<Option<ContactInterval>>, Diagnostic> {
        self.root_between(ContactInterval { lo: 0., hi: 1. }, work)
    }
    fn root_between(
        &self,
        range: ContactInterval,
        work: &Work,
    ) -> Result<Option<Option<ContactInterval>>, Diagnostic> {
        let mut sign = 0.;
        let mut slope = f64::INFINITY;
        for i in 0..32 {
            work.tick()?;
            let Some(e) = self.evaluate(ContactInterval {
                lo: range.lo + (range.hi - range.lo) * (i as f64 / 32.),
                hi: range.lo + (range.hi - range.lo) * ((i + 1) as f64 / 32.),
            }) else {
                return Ok(None);
            };
            let d = e.residual.derivative;
            let this = if d.lo > 0. {
                1.
            } else if d.hi < 0. {
                -1.
            } else {
                return Ok(None);
            };
            if sign != 0. && sign != this {
                return Ok(None);
            }
            sign = this;
            slope = slope.min(d.lo.abs().min(d.hi.abs()));
        }
        let Some(left) = self.evaluate(ContactInterval::point(range.lo)) else {
            return Ok(None);
        };
        let Some(right) = self.evaluate(ContactInterval::point(range.hi)) else {
            return Ok(None);
        };
        let left = left.residual.value;
        let right = right.residual.value;
        if (left.lo > 0. && right.lo > 0.) || (left.hi < 0. && right.hi < 0.) {
            return Ok(Some(None));
        }
        if left.lo <= 0. && left.hi >= 0. || right.lo <= 0. && right.hi >= 0. {
            return Ok(None);
        }
        let mut bracket = range;
        for _ in 0..64 {
            work.tick()?;
            let mid = bracket.midpoint();
            let Some(e) = self.evaluate(ContactInterval::point(mid)) else {
                return Ok(None);
            };
            let f = e.residual.value;
            if f.lo > 0. {
                if sign > 0. {
                    bracket.hi = mid;
                } else {
                    bracket.lo = mid;
                }
            } else if f.hi < 0. {
                if sign > 0. {
                    bracket.lo = mid;
                } else {
                    bracket.hi = mid;
                }
            } else {
                let radius = (f.lo.abs().max(f.hi.abs()) / slope).next_up();
                bracket.lo = bracket.lo.max((mid - radius).next_down());
                bracket.hi = bracket.hi.min((mid + radius).next_up());
                break;
            }
            if bracket.hi - bracket.lo < 1e-10 {
                break;
            }
        }
        if !bracket.finite() || bracket.hi - bracket.lo > 1e-8 {
            return Ok(None);
        }
        Ok(Some(Some(bracket)))
    }
}

/// Resolve or certify emptiness of one constrained flat-roof branch. This does
/// not equate an iteration cycle with absence. Uncovered/nonmonotone geometry
/// leaves the existing numerical diagnostic in control.
fn bounded_roof_contact(
    scene: &Scene,
    s: Point,
    r: Point,
    family: &Family,
    selected: &[Vec<Point>],
    screen: usize,
    band: usize,
    vertical_offset: f64,
    work: &Work,
) -> Result<Option<Option<Route>>, Diagnostic> {
    if family.len() != 1
        || selected.len() != 2
        || selected[0].len() != 3
        || selected[1].len() != 2
        || !scene.faces[family[0]].vertical()
    {
        return Ok(None);
    }
    let face = &scene.faces[family[0]];
    let screen = &scene.buildings[screen];
    if screen.roof.iter().any(|p| p[2] != screen.roof[0][2]) {
        return Ok(None);
    }
    let old = selected[0][1];
    let mut host_edges = vec![];
    for bi in scene.building_index.candidates(old, old) {
        let host = &scene.buildings[bi];
        for i in 0..host.roof.len() {
            let edge = [host.roof[i], host.roof[(i + 1) % host.roof.len()]];
            if edge[0][2] == edge[1][2] && point_segment_distance(old, edge[0], edge[1]) <= 1e-8 {
                host_edges.push(edge);
            }
        }
    }
    if host_edges.len() != 1 {
        return Ok(None);
    }
    let mut unresolved = false;
    for edge in std::iter::once(None).chain(
        (0..screen.roof.len())
            .map(|i| Some([screen.roof[i], screen.roof[(i + 1) % screen.roof.len()]])),
    ) {
        let equation = RoofContactEquation {
            source: s,
            receiver: r,
            origin: face.origin,
            normal: face.normal,
            host_edge: host_edges[0],
            screen_edge: edge,
            lateral_corner: None,
        };
        let Some(root) = equation.root(work)? else {
            unresolved = true;
            continue;
        };
        let Some(root) = root else {
            continue;
        };
        let Some(bounds) = equation.evaluate(root) else {
            unresolved = true;
            continue;
        };
        if let (Some(u), Some(t)) = (bounds.screen_fraction, bounds.screen_parameter) {
            let e = edge.unwrap();
            let parameter_tolerance = 1e-8 / distance(e[0], e[1]);
            let contact: Point = std::array::from_fn(|k| s[k] + bounds.contact[k].value.midpoint());
            let ray_tolerance = 1e-8 / distance(contact, r);
            if u.value.hi < -ray_tolerance
                || u.value.lo > 1. + ray_tolerance
                || t.value.hi < -parameter_tolerance
                || t.value.lo > 1. + parameter_tolerance
            {
                continue;
            }
            if u.value.lo <= 0. || u.value.hi >= 1. || t.value.lo <= 0. || t.value.hi >= 1. {
                unresolved = true;
                continue;
            }
        }
        if edge.is_none() {
            // Certify an open-leg roof crossing over the entire root enclosure,
            // not just its midpoint. This invalidates the no-screen branch.
            let direction: [ContactInterval; 2] = std::array::from_fn(|k| {
                ContactInterval::point(r[k])
                    - ContactInterval::point(s[k])
                    - bounds.contact[k].value
            });
            let span = (direction[0] * direction[0] + direction[1] * direction[1])
                .hi
                .sqrt()
                .next_up();
            let eligible = (0..screen.roof.len()).any(|i| {
                let a = screen.roof[i];
                let b = screen.roof[(i + 1) % screen.roof.len()];
                let dx = ContactInterval::point(b[0]) - ContactInterval::point(a[0]);
                let dy = ContactInterval::point(b[1]) - ContactInterval::point(a[1]);
                let projection = dx * direction[1] - dy * direction[0];
                let lower = if projection.lo > 0. {
                    projection.lo
                } else if projection.hi < 0. {
                    -projection.hi
                } else {
                    0.
                };
                (lower / span).next_down() > 340. / acoustics::FREQUENCIES[band]
            });
            let blocked = (0..screen.roof.len()).any(|i| {
                let roof_edge = [screen.roof[i], screen.roof[(i + 1) % screen.roof.len()]];
                let crossed = RoofContactEquation {
                    screen_edge: Some(roof_edge),
                    ..equation
                };
                let Some(crossing) = crossed.evaluate(root) else {
                    return false;
                };
                let u = crossing.screen_fraction.unwrap().value;
                let t = crossing.screen_parameter.unwrap().value;
                let z = bounds.contact[2].value
                    + u * (ContactInterval::point(r[2])
                        - ContactInterval::point(s[2])
                        - bounds.contact[2].value);
                let contact: Point =
                    std::array::from_fn(|k| s[k] + bounds.contact[k].value.midpoint());
                let ray_tolerance = 1e-7 / distance(contact, r);
                let edge_tolerance = 1e-7 / distance(roof_edge[0], roof_edge[1]);
                let roof = ContactInterval::point(roof_edge[0][2])
                    - ContactInterval::point(s[2])
                    - ContactInterval::point(1e-7);
                u.lo > ray_tolerance
                    && u.hi < 1. - ray_tolerance
                    && t.lo > edge_tolerance
                    && t.hi < 1. - edge_tolerance
                    && z.hi < roof.lo
            });
            if eligible && blocked {
                continue;
            }
        }
        let Some(at) = equation.evaluate(ContactInterval::point(root.midpoint())) else {
            unresolved = true;
            continue;
        };
        let p: Point = std::array::from_fn(|k| s[k] + at.support[k].value.midpoint());
        let contact: Point = std::array::from_fn(|k| s[k] + at.contact[k].value.midpoint());
        if !face.contains(contact) {
            unresolved = true;
            continue;
        }
        let candidate = crate::lateral::candidates_on_supports(
            scene,
            s,
            contact,
            band,
            vertical_offset,
            &selected[0],
            work,
        )?
        .into_iter()
        .find(|v| {
            v.len() == 3
                && distance(v[1], p) <= 1e-7
                && lateral_side(v) == lateral_side(&selected[0])
        });
        let Some(candidate) = candidate else {
            unresolved = true;
            continue;
        };
        let choices = vec![candidate, vec![contact, r]];
        let Some((points, reflections, sections)) = resolve_choices(scene, s, r, family, &choices)?
        else {
            unresolved = true;
            continue;
        };
        let Some(route) = build_route(scene, &points, &reflections, &sections, &[1, 0], band)?
        else {
            unresolved = true;
            continue;
        };
        let at = route.reflections[0].0;
        if distance(route.points[at], contact) > 1e-7 {
            // A different controlling obstacle/route is outside this proof.
            unresolved = true;
            continue;
        }
        if !reflection_directions_settled(scene, &route) {
            unresolved = true;
            continue;
        }
        return Ok(Some(Some(route)));
    }
    Ok((!unresolved).then_some(None))
}

/// Every interior vertex must remain on its authored corner or finite roof edge.
/// `candidates_on_supports` also supplies ordinary alternatives, so matching the
/// nearest mirror neighbours alone does not establish the same longer branch.
fn same_lateral_chain(
    scene: &Scene,
    old: &[Point],
    new: &[Point],
    work: &Work,
) -> Result<bool, Diagnostic> {
    if old.len() != new.len() || lateral_side(old) != lateral_side(new) {
        return Ok(false);
    }
    for (a, b) in old[1..old.len() - 1].iter().zip(&new[1..new.len() - 1]) {
        work.tick()?;
        if !crate::lateral::shares_support(scene, *a, *b, work)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A bounded proposal for the nearest incoming flat-roof edge and outgoing
/// fixed lateral corner, with every remote support retained. Feasible roots
/// require full rebuilding. Only a complete zero-excluding residual cover may
/// reject this constrained branch; uncertainty leaves ordinary iteration intact.
fn bounded_roof_corner_contact(
    scene: &Scene,
    s: Point,
    r: Point,
    family: &Family,
    selected: &[Vec<Point>],
    band: usize,
    vertical_offset: f64,
    work: &Work,
) -> Result<Option<Option<Route>>, Diagnostic> {
    if family.len() != 1
        || selected.len() != 2
        || selected.iter().any(|v| v.len() < 3)
        || !scene.faces[family[0]].vertical()
        || scene.faces[family[0]].normal[2] != 0.
    {
        return Ok(None);
    }
    let corner = selected[1][1];
    let fixed_corner = scene
        .building_index
        .candidates(corner, corner)
        .into_iter()
        .any(|bi| {
            scene.buildings[bi]
                .roof
                .iter()
                .any(|p| horizontal(*p, corner) <= 1e-8 && corner[2] < p[2] - 1e-7)
        });
    if !fixed_corner {
        return Ok(None);
    }
    let old = selected[0][selected[0].len() - 2];
    let mut edges = vec![];
    for bi in scene.building_index.candidates(old, old) {
        work.tick()?;
        let b = &scene.buildings[bi];
        for i in 0..b.roof.len() {
            let e = [b.roof[i], b.roof[(i + 1) % b.roof.len()]];
            if e[0][2] == e[1][2] && point_segment_distance(old, e[0], e[1]) <= 1e-8 {
                edges.push(e);
            }
        }
    }
    if edges.len() != 1 {
        return Ok(None);
    }
    let face = &scene.faces[family[0]];
    let equation = RoofContactEquation {
        source: s,
        receiver: r,
        origin: face.origin,
        normal: face.normal,
        host_edge: edges[0],
        screen_edge: None,
        lateral_corner: Some(corner),
    };
    // Exclude only this known finite-support branch when outward intervals
    // cover the entire parameter range with residual bounds excluding zero.
    // Subdivision exhaustion or a singular evaluation never establishes absence.
    let mut pending = vec![(ContactInterval { lo: 0., hi: 1. }, 0)];
    let mut no_root = true;
    while let Some((range, depth)) = pending.pop() {
        work.tick()?;
        let Some(at) = equation.evaluate(range) else {
            no_root = false;
            break;
        };
        if at.residual.value.lo > 0. || at.residual.value.hi < 0. {
            continue;
        }
        if depth == 12 {
            no_root = false;
            break;
        }
        let mid = range.midpoint();
        pending.push((
            ContactInterval {
                lo: range.lo,
                hi: mid,
            },
            depth + 1,
        ));
        pending.push((
            ContactInterval {
                lo: mid,
                hi: range.hi,
            },
            depth + 1,
        ));
    }
    if no_root {
        return Ok(Some(None));
    }
    let mut roots = vec![];
    if let Some(Some(root)) = equation.root(work)? {
        roots.push(root);
    } else {
        // The residual can turn outside the relevant physical branch. Seek
        // locally monotone enclosures; unknown pieces do not establish absence.
        for i in 0..32 {
            if let Some(Some(root)) = equation.root_between(
                ContactInterval {
                    lo: i as f64 / 32.,
                    hi: (i + 1) as f64 / 32.,
                },
                work,
            )? {
                roots.push(root);
            }
        }
    }
    let edge = sub(edges[0][1], edges[0][0]);
    let old_t = dot(sub(old, edges[0][0]), edge) / dot(edge, edge);
    roots.sort_by(|a, b| {
        (a.midpoint() - old_t)
            .abs()
            .total_cmp(&(b.midpoint() - old_t).abs())
    });
    for root in roots {
        let Some(at) = equation.evaluate(ContactInterval::point(root.midpoint())) else {
            continue;
        };
        let p: Point = std::array::from_fn(|k| s[k] + at.support[k].value.midpoint());
        let contact: Point = std::array::from_fn(|k| s[k] + at.contact[k].value.midpoint());
        if !face.contains(contact) {
            continue;
        }
        let mut choices = vec![];
        for (j, (a, b)) in [(s, contact), (contact, r)].into_iter().enumerate() {
            let mut candidate = None;
            for v in crate::lateral::candidates_on_supports(
                scene,
                a,
                b,
                band,
                vertical_offset,
                &selected[j],
                work,
            )? {
                if same_lateral_chain(scene, &selected[j], &v, work)?
                    && if j == 0 {
                        distance(v[v.len() - 2], p) <= 1e-7
                    } else {
                        horizontal(v[1], corner) <= 1e-7
                    }
                {
                    candidate = Some(v);
                    break;
                }
            }
            let Some(candidate) = candidate else {
                continue;
            };
            choices.push(candidate);
        }
        if choices.len() != 2 {
            continue;
        }
        let Some((points, reflections, sections)) = resolve_choices(scene, s, r, family, &choices)?
        else {
            continue;
        };
        let Some(route) = build_route(scene, &points, &reflections, &sections, &[1, 1], band)?
        else {
            continue;
        };
        if route.points.len() != selected[0].len() + selected[1].len() - 1
            || distance(route.points[route.reflections[0].0], contact) > 1e-7
            || !reflection_directions_settled(scene, &route)
        {
            continue;
        }
        return Ok(Some(Some(route)));
    }
    Ok(None)
}

#[cfg(test)]
mod contact_height_tests {
    use super::*;

    fn scene(offset: Point) -> Scene {
        let shift = |p: Point| add(p, offset);
        Scene::prepare(&json!({"reflectors":[
            {"id":"a","vertices":[shift([0.,-20.,0.]),shift([0.,20.,0.]),shift([0.,20.,30.]),shift([0.,-20.,30.])]},
            {"id":"b","vertices":[shift([20.,-20.,0.]),shift([20.,20.,0.]),shift([20.,20.,30.]),shift([20.,-20.,30.])]}
        ]}), &AtomicBool::new(false)).unwrap()
    }
    fn route(points: Vec<Point>, sections: Vec<usize>, kinds: Vec<u8>) -> Route {
        let reflections = sections
            .iter()
            .skip(1)
            .take(sections.len() - 2)
            .enumerate()
            .map(|(fi, i)| (*i, fi))
            .collect();
        Route {
            points,
            sections,
            kinds,
            reflections,
            diffraction: vec![],
            vertical: [0.; 8],
            lateral: [0.; 8],
            terrain_support: false,
        }
    }
    #[test]
    fn near_unit_height_coupling_is_solved_at_local_and_survey_coordinates() {
        // Equal mirror weights and lateral fractions .9995/.0005 give
        // h=.9995*h+.00025*(4+1), hence h=2.5 independently of trial height.
        for offset in [[0.; 3], [600000., 300000., 200.], [1e6; 3]] {
            let sc = scene(offset);
            let points = [
                [10., -10., 4.],
                [1., 0.99, 0.],
                [0., 0., 10.],
                [1., -0.99, 0.],
                [10., 10., 1.],
            ]
            .map(|p| add(p, offset))
            .to_vec();
            let r = route(points, vec![0, 2, 4], vec![1, 1]);
            let h = mixed_contact_heights(&sc, &r).unwrap();
            assert!((h[0] - offset[2] - 2.5).abs() < 1e-8);
        }
    }
    #[test]
    fn coupled_mirrors_and_fixed_top_support_have_analytical_heights() {
        let sc = scene([0.; 3]);
        // Unfolded lengths 5+20+5 and endpoint rise 8.
        let r = route(
            vec![[5., 0., 2.], [0., 0., 7.], [20., 0., 4.], [15., 0., 10.]],
            vec![0, 1, 2, 3],
            vec![0, 0, 0],
        );
        let h = mixed_contact_heights(&sc, &r).unwrap();
        assert!((h[0] - 10. / 3.).abs() < 1e-12);
        assert!((h[1] - 26. / 3.).abs() < 1e-12);
        // Fixed roof support at (5,-5,4) mirrored across x=0 to receiver (10,10,1).
        let r = route(
            vec![[10., -10., 2.], [5., -5., 4.], [0., 0., 9.], [10., 10., 1.]],
            vec![0, 2, 3],
            vec![0, 0],
        );
        assert!((mixed_contact_heights(&sc, &r).unwrap()[0] - 3.).abs() < 1e-12);
    }
    #[test]
    fn sliding_remote_roof_vertex_does_not_delay_height_solve() {
        for offset in [[0.; 3], [600000., 300000., 200.]] {
            let sc = scene(offset);
            let previous = route(
                [
                    [10., -10., 6.],
                    [5., -7., 5.7], // sliding roof vertex, remote from mirror
                    [1., -1., 5.],  // fixed plan-view lateral corner
                    [0., 0., 4.],
                    [20., 5., 3.],
                    [10., 10., 1.],
                ]
                .map(|p| add(p, offset))
                .to_vec(),
                vec![0, 3, 4, 5],
                vec![1, 0, 0],
            );
            let mut current = previous.clone();
            current.points[1][0] += 0.1;
            current.points[2][2] -= 0.2;
            assert!(height_support_plan_settled(&previous, &current));
            // Corner fraction .9 and mirror weights 20/21, 1/3 give
            // 3*h1-h2=12 and -h1+3*h2=2: h1=19/4, h2=9/4.
            let heights = mixed_contact_heights(&sc, &current).unwrap();
            assert!((heights[0] - offset[2] - 19. / 4.).abs() < 1e-8);
            assert!((heights[1] - offset[2] - 9. / 4.).abs() < 1e-8);
            current.points[2][0] += 0.1;
            assert!(!height_support_plan_settled(&previous, &current));
            current = previous.clone();
            current.points[4][1] += 0.1;
            assert!(!height_support_plan_settled(&previous, &current));
            current = previous.clone();
            current.sections[1] = 2;
            assert!(!height_support_plan_settled(&previous, &current));
        }
    }
    #[test]
    fn contact_extrapolation_solves_a_geometric_sequence_and_declines_linear_drift() {
        for offset in [[0.; 3], [601000., 300600., 180.]] {
            let make = |y, z| {
                route(
                    [[10., -10., 6.], [0., y, z], [10., 10., 1.]]
                        .map(|p| add(p, offset))
                        .to_vec(),
                    vec![0, 1, 2],
                    vec![0, 0],
                )
            };
            // Errors halve each time toward the independent limit (0,3,4).
            let a = make(1., 2.);
            let b = make(2., 3.);
            let c = make(2.5, 3.5);
            let contact = extrapolated_contacts(&a, &b, &c).unwrap()[1];
            assert!(distance(contact, add([0., 3., 4.], offset)) < 1e-8);
            assert!(extrapolated_contacts(&make(1., 2.), &make(2., 3.), &make(3., 4.)).is_none());
        }
    }
    #[test]
    fn a_roof_kink_cannot_also_be_a_specular_wall_reflection() {
        let sc = scene([0.; 3]);
        let mut r = route(
            vec![[10., -10., 2.], [0., 0., 10.], [10., 10., 2.]],
            vec![0, 1, 2],
            vec![0, 0],
        );
        r.diffraction = vec![1];
        // A vertical mirror preserves vertical direction. An upward arrival
        // followed by a downward departure at the fixed roof edge is impossible.
        assert_eq!(completed_reflection_mask(&sc, &r).unwrap(), [false; 8]);
        r.diffraction.clear();
        assert!(completed_reflection_mask(&sc, &r).is_err());
        // A coincident but non-controlling support must not remove a valid ray.
        r.diffraction = vec![1];
        r.points[1][2] = 2.;
        assert!(
            completed_reflection_mask(&sc, &r)
                .unwrap()
                .iter()
                .any(|v| *v)
        );
    }
    #[test]
    fn small_contact_update_still_requires_specular_directions() {
        let sc = scene([0.; 3]);
        let mut r = route(
            vec![[10., -10., 2.], [0., 0., 2.0005], [10., 10., 2.]],
            vec![0, 1, 2],
            vec![0, 0],
        );
        assert_eq!(CONTACT_ENCLOSURE_TOLERANCE_M, 0.01);
        assert!(!reflection_directions_settled(&sc, &r));
        r.points[1][2] = 2.;
        assert!(reflection_directions_settled(&sc, &r));
    }
    #[test]
    fn support_matching_rejects_a_different_roof_edge_and_cancellation() {
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        let sc=Scene::prepare(&json!({"buildings":[{"id":"b","footprint":[[1.,0.,0.],[4.,0.,0.],[4.,4.,0.],[1.,4.,0.]],"height_m":10.}]}),&cancel).unwrap();
        let a = route(
            vec![
                [10., -10., 12.],
                [2., 0., 10.],
                [0., 0., 8.],
                [10., 10., 1.],
            ],
            vec![0, 2, 3],
            vec![1, 0],
        );
        let mut b = a.clone();
        b.points[1] = [3., 0., 10.];
        assert!(same_contact_supports(&sc, &a, &b, &work).unwrap());
        b.points[1] = [4., 1., 10.];
        assert!(!same_contact_supports(&sc, &a, &b, &work).unwrap());
        b.points[1] = [3., 0., 10.];
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            same_contact_supports(&sc, &a, &b, &work).unwrap_err().code,
            "cancelled"
        );
    }
    #[test]
    fn finite_roof_root_enclosures_match_independent_decimal_equations() {
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        let scene = Scene::prepare(
            &serde_json::from_str(include_str!(
                "../tests/fixtures/finite-roof-contact-cycle.json"
            ))
            .unwrap(),
            &cancel,
        )
        .unwrap();
        let host = &scene.buildings[scene.building_by_id["host"]];
        let screen = &scene.buildings[scene.building_by_id["screen"]];
        let face = scene
            .faces
            .iter()
            .find(|f| f.object == "mirror" && f.id == "wall:0:1")
            .unwrap();
        let equation = RoofContactEquation {
            source: scene.sources[0].centre,
            receiver: scene.receivers[0].point,
            origin: face.origin,
            normal: face.normal,
            host_edge: [host.roof[0], host.roof[1]],
            screen_edge: None,
            lateral_corner: None,
        };
        // Standalone 60-digit Decimal finite-edge equations supply these roots;
        // they are not ISO reference values or fitted engine output.
        let expected = [
            0.42725623015484717,
            0.343_644_808_726_219_2,
            0.3023497108525233,
            0.453342280699623,
            0.343602990564775,
        ];
        for (i, t) in expected.into_iter().enumerate() {
            let eq = RoofContactEquation {
                screen_edge: if i == 0 {
                    None
                } else {
                    Some([screen.roof[i - 1], screen.roof[i % 4]])
                },
                ..equation
            };
            let root = eq.root(&work).unwrap().unwrap().unwrap();
            assert!(
                root.lo <= t && t <= root.hi,
                "{i}: {root:?} must enclose {t}"
            );
            if i != 0 {
                let value = eq.evaluate(root).unwrap();
                let u = value.screen_fraction.unwrap().value;
                let v = value.screen_parameter.unwrap().value;
                assert!(u.hi < 0. || u.lo > 1. || v.hi < 0. || v.lo > 1.);
            }
        }
        let flat = RoofContactEquation {
            source: [equation.source[0], equation.source[1], host.roof[0][2]],
            receiver: [equation.receiver[0], equation.receiver[1], host.roof[0][2]],
            ..equation
        };
        assert!(
            flat.root(&work).unwrap().is_none(),
            "zero/nonmonotone derivative is uncertified"
        );
        let singular = RoofContactEquation {
            screen_edge: Some([screen.roof[0], screen.roof[0]]),
            ..equation
        };
        assert!(singular.root(&work).unwrap().is_none());
        let invalid = ContactInterval::point(1.) / ContactInterval { lo: -1., hi: 1. };
        assert!(!invalid.finite());
        assert!(!(ContactInterval::point(0.) * invalid).finite());
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(equation.root(&work).unwrap_err().code, "cancelled");
    }

    #[test]
    fn bounded_roof_contact_keeps_feasible_roots_and_declines_uncovered_geometry() {
        for (fixture, point, contact, feasible) in [
            (
                include_str!("../tests/fixtures/finite-roof-contact-cycle.json"),
                [-47.85383296928013, -178.85988634560965, 36.103200000000015],
                [263.7648114950862, 113.93493147441651, 17.730477182135055],
                false,
            ),
            (
                include_str!("../tests/fixtures/oscillating-roof-contact.json"),
                [-18.73041734849517, -235.44082022730077, 36.103200000000015],
                [263.7648114950862, 94.00015097057344, 24.01010518546991],
                true,
            ),
        ] {
            let cancel = AtomicBool::new(false);
            let operations = AtomicUsize::new(0);
            let work = Work {
                cancel: &cancel,
                operations: &operations,
            };
            let mut scene =
                Scene::prepare(&serde_json::from_str(fixture).unwrap(), &cancel).unwrap();
            let screen = scene.building_by_id["screen"];
            let fi = scene
                .faces
                .iter()
                .position(|f| f.object == "mirror" && f.id == "wall:0:1")
                .unwrap();
            let s = scene.sources[0].centre;
            let r = scene.receivers[0].point;
            let selected = vec![vec![s, point, contact], vec![contact, r]];
            let result =
                bounded_roof_contact(&scene, s, r, &vec![fi], &selected, screen, 5, 100., &work)
                    .unwrap()
                    .expect("this flat finite-edge domain is certified");
            assert_eq!(result.is_some(), feasible);
            if let Some(route) = result {
                assert!(reflection_directions_settled(&scene, &route));
                assert!(distance(route.points[route.reflections[0].0], contact) < 1e-7);
            }
            scene.buildings[screen].roof[0][2] += 1.;
            assert!(
                bounded_roof_contact(&scene, s, r, &vec![fi], &selected, screen, 5, 100., &work)
                    .unwrap()
                    .is_none(),
                "nonflat support is not a certified absence"
            );
        }
    }

    #[test]
    fn roof_corner_proposal_encloses_independent_root_and_preserves_fallback() {
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        let scene = Scene::prepare(
            &serde_json::from_str(include_str!("../tests/fixtures/roof-corner-contact.json"))
                .unwrap(),
            &cancel,
        )
        .unwrap();
        let fi = scene
            .faces
            .iter()
            .position(|f| f.object == "mirror" && f.id == "wall:0:1")
            .unwrap();
        let face = &scene.faces[fi];
        let b = &scene.buildings[scene.building_by_id["screen"]];
        let s = scene.sources[0].centre;
        let r = scene.receivers[0].point;
        let corner = [601192.7187383752, 300780.7606436291, 196.22837974779392];
        let equation = RoofContactEquation {
            source: s,
            receiver: r,
            origin: face.origin,
            normal: face.normal,
            host_edge: [b.roof[3], b.roof[0]],
            screen_edge: None,
            lateral_corner: Some(corner),
        };
        // Standalone 60-digit Decimal equations, not fitted engine output.
        let root = equation
            .root_between(
                ContactInterval {
                    lo: 9. / 32.,
                    hi: 10. / 32.,
                },
                &work,
            )
            .unwrap()
            .unwrap()
            .unwrap();
        let expected_t = 0.2828470891178473;
        assert!(root.lo <= expected_t && expected_t <= root.hi);
        let contact = [601263.7648114951, 300719.6586782064, 198.52684359155728];
        let mut selected = vec![
            vec![
                s,
                [601192.718738375, 300658.55671278347, b.roof[0][2]],
                contact,
            ],
            vec![contact, corner, r],
        ];
        let route = bounded_roof_corner_contact(&scene, s, r, &vec![fi], &selected, 6, 100., &work)
            .unwrap()
            .expect("the independently located roof/corner branch is resolved")
            .expect("the independently located roof/corner route is feasible");
        assert!(reflection_directions_settled(&scene, &route));
        assert!(
            distance(
                route.points[2],
                [601263.7648114951, 300726.11348252563, 198.57210617865197]
            ) < 1e-5
        );
        selected[1][1][0] -= 0.2;
        assert!(
            bounded_roof_corner_contact(&scene, s, r, &vec![fi], &selected, 6, 100., &work)
                .unwrap()
                .is_none(),
            "a noncorner support leaves ordinary search in control"
        );
        selected[1][1] = corner;
        // The adjacent module/cell has no root on this constrained branch.
        // Independent exact-rational polynomial bounds cover its numerator.
        let empty_s = [600718.1069781118, 300600.60565214214, 213.05519390398783];
        let empty_r = [600866.5155394394, 300852.55321920995, 188.20280633860233];
        let empty = bounded_roof_corner_contact(
            &scene,
            empty_s,
            empty_r,
            &vec![fi],
            &selected,
            5,
            100.,
            &work,
        )
        .unwrap();
        assert!(matches!(empty, Some(None)));
        let empty_s = [600687.5439853733, 300623.32511685917, 213.05519390398783];
        let empty_r = [600836.0354784793, 300852.55321920995, 188.50414026033107];
        assert!(matches!(
            bounded_roof_corner_contact(
                &scene,
                empty_s,
                empty_r,
                &vec![fi],
                &selected,
                5,
                100.,
                &work,
            )
            .unwrap(),
            Some(None)
        ));
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            bounded_roof_corner_contact(&scene, s, r, &vec![fi], &selected, 6, 100., &work)
                .unwrap_err()
                .code,
            "cancelled"
        );
    }

    #[test]
    fn long_roof_chain_matches_every_remote_support() {
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        let scene = Scene::prepare(
            &serde_json::from_str(include_str!(
                "../tests/fixtures/roof-chain-corner-valid.json"
            ))
            .unwrap(),
            &cancel,
        )
        .unwrap();
        let fi = scene
            .faces
            .iter()
            .position(|f| f.object == "mirror" && f.id == "wall:0:1")
            .unwrap();
        let s = scene.sources[0].centre;
        let r = scene.receivers[0].point;
        let e = [600828.898742849, 300591.81838487723, 210.88273074518745];
        let p = [601192.718738375, 300658.55671278347, 200.82530743532906];
        let c = [601263.7648114951, 300719.6586782064, 198.52684359155728];
        let q = [601192.7187383752, 300780.7606436291, 196.22837974779392];
        let mut selected = vec![vec![s, e, p, c], vec![c, q, r]];
        let solved =
            bounded_roof_corner_contact(&scene, s, r, &vec![fi], &selected, 6, 100., &work)
                .unwrap()
                .unwrap()
                .unwrap();
        assert_eq!(solved.points.len(), 6);
        assert!(distance(solved.points[1], [e[0], 300589.81838487723, e[2]]) < 1e-5);
        assert!(
            distance(
                solved.points[3],
                [c[0], 300726.11348252563, 198.57210617865197]
            ) < 1e-5
        );
        selected[0][1][0] -= 0.2;
        assert!(
            bounded_roof_corner_contact(&scene, s, r, &vec![fi], &selected, 6, 100., &work)
                .unwrap()
                .is_none(),
            "a changed remote support cannot be replaced by an ordinary alternative"
        );
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            bounded_roof_corner_contact(&scene, s, r, &vec![fi], &selected, 6, 100., &work)
                .unwrap_err()
                .code,
            "cancelled"
        );
    }

    #[test]
    fn joint_height_route_preserves_fixed_terrain_support_and_specular_direction() {
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        let scene = Scene::prepare(
            &serde_json::from_str(include_str!(
                "../tests/fixtures/fixed-support-height-contact.json"
            ))
            .unwrap(),
            &cancel,
        )
        .unwrap();
        let fi = scene
            .faces
            .iter()
            .position(|f| f.object == "mirror" && f.id == "wall:1:0")
            .unwrap();
        let s = scene.sources[0].centre;
        let r = scene.receivers[0].point;
        let expected = [
            [600843.9519099166, 300467.08706518204, 212.94408001261175],
            [600807.541915685, 300529.823772226, 212.07364724058586],
            [600826.8084221098, 300525.24062891095, 211.836],
        ];
        let mut a = expected[0];
        a[2] += 0.1 * 0.1132028618904038;
        let mut c = expected[1];
        c[2] += 0.1;
        let mut trial = route(vec![s, a, c, expected[2], r], vec![0, 2, 4], vec![1, 0]);
        trial.reflections = vec![(2, fi)];
        trial.diffraction = vec![1, 3];
        trial.terrain_support = true;
        let solved = fixed_support_height_route(&scene, s, r, &vec![fi], &trial, 6, 100., &work)
            .unwrap()
            .expect("joint solve retains the controlling ground ridge");
        for (actual, expected) in solved.points[1..4].iter().zip(expected) {
            assert!(distance(*actual, expected) < 1e-5);
        }
        assert!(reflection_directions_settled(&scene, &solved));
        trial.diffraction.push(2);
        assert!(
            fixed_support_height_route(&scene, s, r, &vec![fi], &trial, 6, 100., &work)
                .unwrap()
                .is_none(),
            "a pinned mirror must not be freely moved"
        );
        trial.diffraction.pop();
        trial.points[3][2] -= 10.;
        assert!(
            fixed_support_height_route(&scene, s, r, &vec![fi], &trial, 6, 100., &work)
                .unwrap()
                .is_none(),
            "a downward detour is not a controlling vertical support"
        );
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            fixed_support_height_route(&scene, s, r, &vec![fi], &trial, 6, 100., &work)
                .unwrap_err()
                .code,
            "cancelled"
        );
    }

    #[test]
    fn singular_height_equations_decline_acceleration() {
        let sc = scene([0.; 3]);
        // Both corners project to the bounce station, yielding h=h.
        let r = route(
            vec![
                [10., -10., 4.],
                [1., 1., 2.],
                [0., 0., 2.],
                [1., -1., 2.],
                [10., 10., 1.],
            ],
            vec![0, 2, 4],
            vec![1, 1],
        );
        assert!(mixed_contact_heights(&sc, &r).is_none());
    }
}
