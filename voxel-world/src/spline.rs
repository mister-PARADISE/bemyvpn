//! Monotone cubic splines (Fritsch–Carlson) and *nested* splines.
//! FROZEN shared file.
//!
//! A nested spline maps a vector of input coordinates (e.g. [continentalness, erosion, pv])
//! to a scalar. Each control point's value is either a constant or another spline evaluated on
//! a different coordinate — so the terrain shape is a smooth piecewise-cubic function of several
//! climate parameters. Interpolation between control points uses monotone cubic Hermite
//! tangents computed from the (already evaluated) neighbouring values, so the curve never
//! overshoots between points and is C1-continuous.

#[derive(Clone, Debug)]
pub enum SplineValue {
    Const(f32),
    Nested(Box<Spline>),
}

impl From<f32> for SplineValue {
    fn from(v: f32) -> Self {
        SplineValue::Const(v)
    }
}
impl From<Spline> for SplineValue {
    fn from(s: Spline) -> Self {
        SplineValue::Nested(Box::new(s))
    }
}

#[derive(Clone, Debug)]
pub struct Spline {
    /// Index into the `inputs` slice passed to `eval`.
    pub coord: usize,
    /// Control points sorted by location.
    pub points: Vec<(f32, SplineValue)>,
}

impl Spline {
    /// Build from (location, value) pairs; locations must be strictly increasing.
    pub fn new(coord: usize, points: Vec<(f32, SplineValue)>) -> Self {
        assert!(!points.is_empty());
        for w in points.windows(2) {
            assert!(w[0].0 < w[1].0, "spline locations must increase");
        }
        Spline { coord, points }
    }

    /// Convenience: a 1D spline of constants.
    pub fn consts(coord: usize, pts: &[(f32, f32)]) -> Self {
        Spline::new(coord, pts.iter().map(|&(x, y)| (x, SplineValue::Const(y))).collect())
    }

    #[inline]
    fn value_at(&self, i: usize, inputs: &[f32]) -> f32 {
        match &self.points[i].1 {
            SplineValue::Const(v) => *v,
            SplineValue::Nested(s) => s.eval(inputs),
        }
    }

    /// Evaluate. Outside the control range the curve is held constant (clamped).
    pub fn eval(&self, inputs: &[f32]) -> f32 {
        let x = inputs[self.coord];
        let p = &self.points;
        let n = p.len();
        if n == 1 || x <= p[0].0 {
            return self.value_at(0, inputs);
        }
        if x >= p[n - 1].0 {
            return self.value_at(n - 1, inputs);
        }
        // Find segment k with p[k].0 <= x < p[k+1].0 (binary search; specs are small).
        let mut lo = 0usize;
        let mut hi = n - 1;
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if p[mid].0 <= x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let k = lo;
        let x0 = p[k].0;
        let x1 = p[k + 1].0;
        let y0 = self.value_at(k, inputs);
        let y1 = self.value_at(k + 1, inputs);
        let h = x1 - x0;
        let d = (y1 - y0) / h; // secant of this segment

        // Neighbour secants (evaluated lazily; only 4 control values touched in total).
        let d_prev = if k > 0 {
            let xm = p[k - 1].0;
            let ym = self.value_at(k - 1, inputs);
            Some((y0 - ym) / (x0 - xm))
        } else {
            None
        };
        let d_next = if k + 2 < n {
            let xp = p[k + 2].0;
            let yp = self.value_at(k + 2, inputs);
            Some((yp - y1) / (xp - x1))
        } else {
            None
        };

        let m0 = monotone_tangent(d_prev, d);
        let m1 = monotone_tangent(Some(d), d_next.unwrap_or(d)).min_abs_limit(d);
        let m0 = m0.min_abs_limit(d);

        let t = (x - x0) / h;
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        h00 * y0 + h10 * h * m0 + h01 * y1 + h11 * h * m1
    }
}

/// Fritsch–Carlson tangent at a knot between secants `a` (left, optional) and `b` (right).
#[inline]
fn monotone_tangent(a: Option<f32>, b: f32) -> f32 {
    match a {
        None => b,
        Some(a) => {
            if a * b <= 0.0 {
                0.0
            } else {
                // Harmonic mean keeps the interpolant monotone.
                2.0 * a * b / (a + b)
            }
        }
    }
}

trait LimitExt {
    fn min_abs_limit(self, secant: f32) -> f32;
}
impl LimitExt for f32 {
    /// Fritsch–Carlson sufficient condition: |m| <= 3|secant| (and same sign or zero).
    #[inline]
    fn min_abs_limit(self, secant: f32) -> f32 {
        if secant == 0.0 {
            return 0.0;
        }
        if self * secant < 0.0 {
            return 0.0;
        }
        let lim = 3.0 * secant.abs();
        self.clamp(-lim, lim)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_knots_and_is_monotone() {
        let s = Spline::consts(0, &[(-1.0, -50.0), (-0.2, -10.0), (0.0, 2.0), (0.3, 40.0), (1.0, 90.0)]);
        for &(x, y) in &[(-1.0f32, -50.0f32), (-0.2, -10.0), (0.0, 2.0), (0.3, 40.0), (1.0, 90.0)] {
            assert!((s.eval(&[x]) - y).abs() < 1e-3);
        }
        let mut prev = f32::MIN;
        for i in 0..=2000 {
            let x = -1.2 + 2.4 * i as f32 / 2000.0;
            let v = s.eval(&[x]);
            assert!(v >= prev - 1e-4, "non-monotone at {x}");
            prev = v;
        }
    }

    #[test]
    fn continuity_and_nesting() {
        let inner_a = Spline::consts(1, &[(-1.0, 100.0), (1.0, 0.0)]);
        let inner_b = Spline::consts(1, &[(-1.0, 10.0), (1.0, 5.0)]);
        let s = Spline::new(0, vec![(-0.5, inner_a.into()), (0.5, inner_b.into())]);
        let mut prev = s.eval(&[-0.6, 0.0]);
        for i in 1..=4000 {
            let x = -0.6 + 1.2 * i as f32 / 4000.0;
            let v = s.eval(&[x, 0.0]);
            assert!((v - prev).abs() < 0.2, "jump at {x}");
            prev = v;
        }
        assert!((s.eval(&[-0.5, -1.0]) - 100.0).abs() < 1e-3);
        assert!((s.eval(&[0.5, 1.0]) - 5.0).abs() < 1e-3);
    }
}
