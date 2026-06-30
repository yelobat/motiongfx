use alloc::vec::Vec;

use peniko::kurbo::{
    BezPath, CubicBez, ParamCurve, ParamCurveArclen, ParamCurveArea,
    PathEl, Point, Shape,
};

/// Accuracy used for the arclength quadrature that weights
/// subdivision.
const ARCLEN_ACCURACY: f64 = 1e-3;

/// A precomputed point-level correspondence between two paths.
#[derive(Debug, Clone)]
pub struct PathMorph {
    pairs: Vec<SubpathPair>,
}

/// One subpath pair.
#[derive(Debug, Clone)]
struct SubpathPair {
    from: Vec<CubicBez>,
    to: Vec<CubicBez>,
    closed: bool,
}

/// A decomposed subpath.
struct Subpath {
    segs: Vec<CubicBez>,
    closed: bool,
}

impl Subpath {
    /// Enclosed signed area.
    fn signed_area(&self) -> f64 {
        self.segs.iter().map(|c| c.signed_area()).sum()
    }
}

impl PathMorph {
    pub fn new(from: &BezPath, to: &BezPath) -> Self {
        let mut a = subpaths(from);
        let mut b = subpaths(to);
        let by_area_desc = |s: &mut Vec<Subpath>| {
            s.sort_by(|x, y| {
                y.signed_area()
                    .abs()
                    .total_cmp(&x.signed_area().abs())
            });
        };
        by_area_desc(&mut a);
        by_area_desc(&mut b);

        let center = |p: &BezPath| {
            (!p.is_empty()).then(|| p.bounding_box().center())
        };
        let center_a =
            center(from).or_else(|| center(to)).unwrap_or_default();
        let center_b =
            center(to).or_else(|| center(from)).unwrap_or_default();

        let count = a.len().max(b.len());
        let mut pairs = Vec::with_capacity(count);
        let mut a = a.into_iter();
        let mut b = b.into_iter();
        for _ in 0..count {
            let pair = match (a.next(), b.next()) {
                (Some(sa), Some(sb)) => make_pair(sa, sb),
                (Some(sa), None) => {
                    let point = point_subpath(center_b, sa.closed);
                    make_pair(sa, point)
                }
                (None, Some(sb)) => {
                    let point = point_subpath(center_a, sb.closed);
                    make_pair(point, sb)
                }
                (None, None) => break,
            };
            pairs.push(pair);
        }
        Self { pairs }
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    pub fn lerp(&self, t: f64) -> BezPath {
        let t = t.clamp(0.0, 1.0);
        let mut path = BezPath::new();
        for pair in &self.pairs {
            let (Some(fa), Some(fb)) =
                (pair.from.first(), pair.to.first())
            else {
                continue;
            };
            path.move_to(fa.p0.lerp(fb.p0, t));
            for (ca, cb) in pair.from.iter().zip(&pair.to) {
                path.curve_to(
                    ca.p1.lerp(cb.p1, t),
                    ca.p2.lerp(cb.p2, t),
                    ca.p3.lerp(cb.p3, t),
                );
            }
            if pair.closed {
                path.close_path();
            }
        }
        path
    }
}

fn subpaths(path: &BezPath) -> Vec<Subpath> {
    let mut out = Vec::new();
    let mut segs: Vec<CubicBez> = Vec::new();
    let mut start = Point::ZERO;
    let mut cursor = Point::ZERO;
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                flush(&mut out, &mut segs, false);
                start = p;
                cursor = p;
            }
            PathEl::LineTo(p) => {
                segs.push(line_cubic(cursor, p));
                cursor = p;
            }
            PathEl::QuadTo(p1, p2) => {
                segs.push(
                    peniko::kurbo::QuadBez::new(cursor, p1, p2)
                        .raise(),
                );
                cursor = p2;
            }
            PathEl::CurveTo(p1, p2, p3) => {
                segs.push(CubicBez::new(cursor, p1, p2, p3));
                cursor = p3;
            }
            PathEl::ClosePath => {
                if cursor != start {
                    segs.push(line_cubic(cursor, start));
                }
                flush(&mut out, &mut segs, true);
                cursor = start;
            }
        }
    }
    flush(&mut out, &mut segs, false);
    out
}

fn flush(
    out: &mut Vec<Subpath>,
    segs: &mut Vec<CubicBez>,
    closed: bool,
) {
    if !segs.is_empty() {
        out.push(Subpath {
            segs: core::mem::take(segs),
            closed,
        })
    }
}

fn line_cubic(p0: Point, p1: Point) -> CubicBez {
    CubicBez::new(
        p0,
        p0.lerp(p1, 1.0 / 3.0),
        p0.lerp(p1, 2.0 / 3.0),
        p1,
    )
}

fn point_subpath(p: Point, closed: bool) -> Subpath {
    Subpath {
        segs: alloc::vec![CubicBez::new(p, p, p, p)],
        closed,
    }
}

fn make_pair(sa: Subpath, sb: Subpath) -> SubpathPair {
    let closed = sa.closed || sb.closed;
    let mut from = sa.segs;
    let mut to = sb.segs;

    if closed
        && sa.closed
        && sb.closed
        && cubics_area(&from) * cubics_area(&to) < 0.0
    {
        reverse_cubics(&mut to);
    }

    let target = from.len().max(to.len());
    subdivide_to(&mut from, target);
    subdivide_to(&mut to, target);

    if closed && from.len() == to.len() && from.len() > 1 {
        let offset = best_rotation(&from, &to);
        to.rotate_left(offset);
    }

    SubpathPair { from, to, closed }
}

fn cubics_area(segs: &[CubicBez]) -> f64 {
    segs.iter().map(|c| c.signed_area()).sum()
}

fn reverse_cubics(segs: &mut [CubicBez]) {
    segs.reverse();
    for c in segs.iter_mut() {
        *c = CubicBez::new(c.p3, c.p2, c.p1, c.p0);
    }
}

fn subdivide_to(segs: &mut Vec<CubicBez>, target: usize) {
    let n = segs.len();
    if n == 0 || target <= n {
        return;
    }
    let extra = target - n;

    let lengths: Vec<f64> =
        segs.iter().map(|c| c.arclen(ARCLEN_ACCURACY)).collect();
    let total: f64 = lengths.iter().sum();

    let mut splits: Vec<usize>;
    if total <= 0.0 {
        splits = (0..n)
            .map(|i| extra / n + usize::from(i < extra % n))
            .collect();
    } else {
        let exact: Vec<f64> = lengths
            .iter()
            .map(|len| extra as f64 * len / total)
            .collect();
        splits = exact.iter().map(|e| *e as usize).collect();
        let assigned: usize = splits.iter().sum();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&i, &j| {
            let fi = exact[i] - exact[i].floor();
            let fj = exact[j] - exact[j].floor();
            fj.partial_cmp(&fi).unwrap_or(core::cmp::Ordering::Equal)
        });
        for &i in order.iter().take(extra - assigned) {
            splits[i] += 1;
        }
    }

    let mut out = Vec::with_capacity(target);
    for (seg, &k) in segs.iter().zip(&splits) {
        let pieces = k + 1;
        let mut t0 = 0.0;
        for p in 0..pieces {
            let t1 = (p + 1) as f64 / pieces as f64;
            out.push(seg.subsegment(t0..t1));
            t0 = t1;
        }
    }
    *segs = out;
}

fn best_rotation(from: &[CubicBez], to: &[CubicBez]) -> usize {
    let n = from.len();
    let cost = |offset: usize| -> f64 {
        from.iter()
            .enumerate()
            .map(|(i, c)| {
                c.p0.distance_squared(to[(i + offset) % n].p0)
            })
            .sum()
    };
    (0..n)
        .min_by(|&x, &y| cost(x).total_cmp(&cost(y)))
        .unwrap_or(0)
}
