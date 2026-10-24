// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Bounded f64 ramp/charge models, independently derived from RC circuit equations.
//! STA-REQ-0721, STA-REQ-0722, STA-REQ-0726, STA-REQ-0728, STA-REQ-0729. All times ns,
//! capacitances pF, resistances kOhm.
use crate::ComputeError;

/// Progress-normalized trip points: falling voltages are mapped through 1-v.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    pub lower: f64,
    pub upper: f64,
    pub input: f64,
    pub output: f64,
    pub derate: f64,
}
impl Thresholds {
    pub fn validate(self) -> Result<(), ComputeError> {
        let valid = [self.lower, self.upper, self.input, self.output]
            .iter()
            .all(|x| x.is_finite() && *x > 0.0 && *x < 1.0);
        if !valid
            || self.lower >= self.upper
            || !self.derate.is_finite()
            || self.derate <= 0.0
            || self.derate > 1.0
        {
            return Err(ComputeError::Invalid("RC trip points/derate".into()));
        }
        Ok(())
    }
}

/// Passive moment-matched driving-point pi.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Pi {
    pub near: f64,
    pub far: f64,
    pub resistance: f64,
}
impl Pi {
    pub fn total(self) -> f64 {
        self.near + self.far
    }
    pub fn validate(self) -> Result<(), ComputeError> {
        if [self.near, self.far, self.resistance, self.total()]
            .iter()
            .any(|x| !x.is_finite() || *x < 0.0)
        {
            return Err(ComputeError::Invalid("nonpassive RC pi".into()));
        }
        Ok(())
    }
}

/// Fraction of a far capacitance charged during a ramp interval u, for tau=R*C.
pub fn charged_fraction(u: f64, tau: f64) -> f64 {
    if tau == 0.0 {
        return 1.0;
    }
    if u == 0.0 {
        return 0.0;
    }
    let x = u / tau;
    if x < 1e-3 {
        // Cancellation-free series of 1-(1-exp(-x))/x.
        x * (0.5 + x * (-1.0 / 6.0 + x * (1.0 / 24.0 + x * (-1.0 / 120.0 + x / 720.0))))
    } else {
        1.0 + (-x).exp_m1() / x
    }
}

/// A fitted finite-ramp Thevenin driver and passive pi response.
#[derive(Debug, Clone, Copy)]
pub struct GateResponse {
    pub delay: f64,
    pub slew: f64,
    pub capacitance: f64,
    pub resistance: f64,
    pub duration: f64,
    pub root_crossing: f64,
}

fn fit_duration(slew: f64, cap: f64, res: f64, t: Thresholds) -> Result<f64, ComputeError> {
    let target = slew * t.derate * (t.output - t.lower) / (t.upper - t.lower);
    let tau = res * cap;
    let minimum = tau * ((1.0 - t.lower) / (1.0 - t.output)).ln();
    if target <= minimum {
        return Err(ComputeError::Invalid(
            "NLDM transition incompatible with Thevenin resistance".into(),
        ));
    }
    let mut lo = 0.0;
    let mut hi = slew * t.derate / (t.upper - t.lower) + 2.0 * tau;
    let mut x = (target / (t.output - t.lower)).min(hi);
    let tolerance = (hi * 1e-12).max(1e-15);
    for _ in 0..56 {
        let mid = crossing(x, tau, t.output);
        let lower = crossing(x, tau, t.lower);
        let residual = mid - lower - target;
        if residual.abs() <= tolerance {
            return Ok(x);
        }
        if residual < 0.0 {
            lo = x;
        } else {
            hi = x;
        }
        let slope =
            crossing_gradient(x, tau, t.output, mid) - crossing_gradient(x, tau, t.lower, lower);
        let next = x - residual / slope;
        x = if next > lo && next < hi {
            next
        } else {
            lo + (hi - lo) * 0.5
        };
    }
    Err(ComputeError::Invalid(
        "RC source-duration fit did not converge".into(),
    ))
}

#[derive(Debug, Clone, Copy)]
struct Poles {
    a: f64,
    b: f64,
    root_weight: f64,
}
impl Poles {
    fn new(pi: Pi, r: f64) -> Self {
        let sum = r * pi.total() + pi.resistance * pi.far;
        let product = r * pi.resistance * pi.near * pi.far;
        let disc = (sum * sum - 4.0 * product).max(0.0).sqrt();
        let a = (sum + disc) * 0.5;
        let b = if a > 0.0 { product / a } else { 0.0 };
        let root_weight = if a > b {
            (a - pi.resistance * pi.far) / (a - b)
        } else {
            1.0
        };
        Self { a, b, root_weight }
    }
    fn root(self, x: f64, d: f64) -> f64 {
        self.root_weight * ramp_voltage(x, d, self.a)
            + (1.0 - self.root_weight) * ramp_voltage(x, d, self.b)
    }
    fn far(self, x: f64, d: f64) -> f64 {
        if self.a == self.b {
            return self.root(x, d);
        }
        (self.a * ramp_voltage(x, d, self.a) - self.b * ramp_voltage(x, d, self.b))
            / (self.a - self.b)
    }
    fn crossing(self, d: f64, p: f64) -> f64 {
        let mut lo = 0.0;
        let mut hi = d - self.a * (1.0 - p).ln();
        let tolerance = (hi * 1e-13).max(1e-16);
        for _ in 0..80 {
            if hi - lo <= tolerance {
                break;
            }
            let mid = lo + (hi - lo) * 0.5;
            if self.root(mid, d) < p {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo + (hi - lo) * 0.5
    }
}

/// NLDM-compatible Thevenin proxy: the delay sensitivity supplies a tail
/// resistance, with the threshold log converting its time reference. It is an
/// explicit NLDM parameter estimate, not a transistor DC characterization.
/// Match the lower/delay trip points; equate charge over the source's active ramp.
pub fn gate_detail(
    pi: Pi,
    t: Thresholds,
    mut eval: impl FnMut(f64) -> (f64, f64),
) -> Result<GateResponse, ComputeError> {
    pi.validate()?;
    t.validate()?;
    if t.output <= t.lower || t.output >= t.upper {
        return Err(ComputeError::Invalid(
            "RC driver delay threshold must lie within slew trip points".into(),
        ));
    }
    let total = pi.total();
    let (d0, s0) = eval(total);
    if !d0.is_finite() || !s0.is_finite() {
        return Err(ComputeError::Invalid("RC NLDM nonfinite".into()));
    }
    if pi.resistance == 0.0 || pi.far == 0.0 || s0 <= 0.0 {
        return Ok(GateResponse {
            delay: d0,
            slew: s0.max(0.0),
            capacitance: total,
            resistance: 0.0,
            duration: s0.max(0.0) * t.derate / (t.upper - t.lower),
            root_crossing: s0.max(0.0) * t.derate / (t.upper - t.lower) * t.output,
        });
    }
    let delta = (total * 0.01).max(1e-12);
    let lower = (total - delta).max(0.0);
    let derivative = (eval(total + delta).0 - eval(lower).0) / (total + delta - lower);
    let res = derivative.max(0.0) * (-(1.0 - t.output).ln());
    solve_driver(pi, t, res, eval)
}

/// [`gate_detail`] with the caller's Thevenin resistance `res` (kOhm) instead of the central
/// delay sensitivity over 1 % of the total load: the same validation, degenerate lumped loads,
/// charge solve and waveform. The caller states how `res` derives from the NLDM tables.
pub fn gate_detail_with_resistance(
    pi: Pi,
    t: Thresholds,
    res: f64,
    mut eval: impl FnMut(f64) -> (f64, f64),
) -> Result<GateResponse, ComputeError> {
    pi.validate()?;
    t.validate()?;
    if t.output <= t.lower || t.output >= t.upper {
        return Err(ComputeError::Invalid(
            "RC driver delay threshold must lie within slew trip points".into(),
        ));
    }
    let total = pi.total();
    let (d0, s0) = eval(total);
    if !d0.is_finite() || !s0.is_finite() {
        return Err(ComputeError::Invalid("RC NLDM nonfinite".into()));
    }
    if pi.resistance == 0.0 || pi.far == 0.0 || s0 <= 0.0 {
        return Ok(GateResponse {
            delay: d0,
            slew: s0.max(0.0),
            capacitance: total,
            resistance: 0.0,
            duration: s0.max(0.0) * t.derate / (t.upper - t.lower),
            root_crossing: s0.max(0.0) * t.derate / (t.upper - t.lower) * t.output,
        });
    }
    solve_driver(pi, t, res, eval)
}

/// The bounded charge solve and waveform of [`gate_detail`] for a non-degenerate pi.
fn solve_driver(
    pi: Pi,
    t: Thresholds,
    res: f64,
    mut eval: impl FnMut(f64) -> (f64, f64),
) -> Result<GateResponse, ComputeError> {
    let total = pi.total();
    if !res.is_finite() || res <= 0.0 {
        return Err(ComputeError::Invalid(
            "NLDM has no positive delay sensitivity for RC driver".into(),
        ));
    }
    let poles = Poles::new(pi, res);
    let mut lo = pi.near;
    let mut hi = total;
    let tolerance = (total * 1e-11).max(1e-16);
    for _ in 0..64 {
        let effective = lo + (hi - lo) * 0.5;
        let (_, s) = eval(effective);
        if !s.is_finite() || s <= 0.0 {
            return Err(ComputeError::Invalid(
                "invalid NLDM transition for RC fitting".into(),
            ));
        }
        let duration = fit_duration(s, effective, res, t)?;
        let actual =
            pi.near * poles.root(duration, duration) + pi.far * poles.far(duration, duration);
        let nominal = effective * ramp_voltage(duration, duration, res * effective);
        if nominal > actual {
            hi = effective;
        } else {
            lo = effective;
        }
        if hi - lo <= tolerance {
            break;
        }
    }
    if hi - lo > tolerance {
        return Err(ComputeError::Invalid(
            "RC charge solve did not converge".into(),
        ));
    }
    let effective = lo + (hi - lo) * 0.5;
    let (delay, s) = eval(effective);
    let duration = fit_duration(s, effective, res, t)?;
    let nominal = effective * ramp_voltage(duration, duration, res * effective);
    let actual = pi.near * poles.root(duration, duration) + pi.far * poles.far(duration, duration);
    if (nominal - actual).abs() > total.max(1e-16) * 1e-8 {
        return Err(ComputeError::Invalid("RC charge residual exceeded".into()));
    }
    let slew = (poles.crossing(duration, t.upper) - poles.crossing(duration, t.lower)) / t.derate;
    let root_crossing = poles.crossing(duration, t.output);
    if [delay, slew, duration, root_crossing]
        .iter()
        .any(|v| !v.is_finite())
    {
        return Err(ComputeError::Invalid("RC waveform overflow".into()));
    }
    Ok(GateResponse {
        delay,
        slew,
        capacitance: effective,
        resistance: res,
        duration,
        root_crossing,
    })
}

/// The relative load step of the forward table slopes in [`gate_one_step`].
const SLOPE_STEP: f64 = 1e-7;

/// [`ramp_voltage`] at `time` and its partial derivatives with respect to the time, the ramp
/// duration and the pole time, for a positive duration and pole time.
fn ramp_partials(time: f64, duration: f64, tau: f64) -> [f64; 4] {
    if time <= 0.0 {
        return [0.0; 4];
    }
    if time <= duration {
        let u = time / tau;
        let v = ramp_voltage(time, duration, tau);
        // (1 - e^-u) - u e^-u without cancellation for small u.
        let q = if u < 1e-3 {
            u * u * (0.5 - u / 3.0 + u * u / 8.0)
        } else {
            -(-u).exp_m1() - u * (-u).exp()
        };
        return [v, -(-u).exp_m1() / duration, -v / duration, -q / duration];
    }
    let a = (-(time - duration) / tau).exp();
    let b = (-time / tau).exp();
    [
        1.0 - tau / duration * (a - b),
        (a - b) / duration,
        tau / (duration * duration) * (a - b) - a / duration,
        -(a - b) / duration - (a * (time - duration) - b * time) / (tau * duration),
    ]
}

/// [`ramp_voltage`] at the end of the ramp and its derivative with respect to the duration.
fn settled(duration: f64, tau: f64) -> (f64, f64) {
    if tau == 0.0 {
        return (1.0, 0.0);
    }
    let x = duration / tau;
    (
        ramp_voltage(duration, duration, tau),
        tau / (duration * duration) * -(-x).exp_m1() - (-x).exp() / duration,
    )
}

/// Solves `a x = b` by Gaussian elimination with partial pivoting.
fn solve3(mut a: [[f64; 3]; 3], mut b: [f64; 3]) -> Option<[f64; 3]> {
    for i in 0..3 {
        let p = (i..3).max_by(|&r, &s| a[r][i].abs().total_cmp(&a[s][i].abs()))?;
        a.swap(i, p);
        b.swap(i, p);
        if !a[i][i].is_normal() {
            return None;
        }
        for r in i + 1..3 {
            let f = a[r][i] / a[i][i];
            for k in i..3 {
                a[r][k] -= f * a[i][k];
            }
            b[r] -= f * b[i];
        }
    }
    let mut x = [0.0; 3];
    for i in (0..3).rev() {
        let tail: f64 = (i + 1..3).map(|k| a[i][k] * x[k]).sum();
        x[i] = (b[i] - tail) / a[i][i];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

/// One Newton-Raphson correction of the effective capacitance of a finite-ramp Thevenin driver
/// with the caller's resistance `res` (kOhm), instead of the converged charge solve of
/// [`gate_detail_with_resistance`]. The unknowns are the source start, the source duration and
/// the capacitance; the equations put the single-capacitance response at the lower and the
/// delay trip points at the NLDM times of that capacitance and match the charge of the pi at the
/// end of the source ramp. The step starts from the total load with the source ramp of the
/// table transition at the total load, uses the exact Jacobian (forward table slopes over a
/// relative load step of 1e-7) and is clamped to `[near, total]`. The delay is the NLDM delay at
/// the corrected capacitance; the transition and the root crossing are those of the pi root for
/// the source duration fitted at that capacitance. Degenerate lumped loads are as in
/// [`gate_detail_with_resistance`].
pub fn gate_one_step(
    pi: Pi,
    t: Thresholds,
    res: f64,
    mut eval: impl FnMut(f64) -> (f64, f64),
) -> Result<GateResponse, ComputeError> {
    pi.validate()?;
    t.validate()?;
    if t.output <= t.lower || t.output >= t.upper {
        return Err(ComputeError::Invalid(
            "RC driver delay threshold must lie within slew trip points".into(),
        ));
    }
    let total = pi.total();
    let (d0, s0) = eval(total);
    if !d0.is_finite() || !s0.is_finite() {
        return Err(ComputeError::Invalid("RC NLDM nonfinite".into()));
    }
    let span = t.upper - t.lower;
    if pi.resistance == 0.0 || pi.far == 0.0 || s0 <= 0.0 {
        return Ok(GateResponse {
            delay: d0,
            slew: s0.max(0.0),
            capacitance: total,
            resistance: 0.0,
            duration: s0.max(0.0) * t.derate / span,
            root_crossing: s0.max(0.0) * t.derate / span * t.output,
        });
    }
    if !res.is_finite() || res <= 0.0 {
        return Err(ComputeError::Invalid(
            "NLDM has no positive delay sensitivity for RC driver".into(),
        ));
    }
    let step = total * SLOPE_STEP;
    let (d1, s1) = eval(total + step);
    let (dd, ds) = ((d1 - d0) / step, (s1 - s0) / step);
    let duration = s0 * t.derate / span;
    let lead = t.derate * (t.output - t.lower) / span;
    let theta = res * total;
    // Start: source ramp of the table transition at the total load, its trip time at the delay.
    let p1 = ramp_partials(t.output * duration, duration, theta);
    let p2 = ramp_partials(t.output * duration - lead * s0, duration, theta);
    let poles = Poles::new(pi, res);
    let (ga, gpa) = settled(duration, poles.a);
    let (gb, gpb) = settled(duration, poles.b);
    let w = poles.root_weight;
    let (root, root_p) = (w * ga + (1.0 - w) * gb, w * gpa + (1.0 - w) * gpb);
    let (far, far_p) = if poles.a == poles.b {
        (root, root_p)
    } else {
        (
            (poles.a * ga - poles.b * gb) / (poles.a - poles.b),
            (poles.a * gpa - poles.b * gpb) / (poles.a - poles.b),
        )
    };
    let charge = pi.near * root + pi.far * far;
    let charge_p = pi.near * root_p + pi.far * far_p;
    let e = ramp_partials(duration, duration, theta);
    let residual = [
        p1[0] - t.output,
        p2[0] - t.lower,
        (total * e[0] - charge) / total,
    ];
    let jacobian = [
        [-p1[1], p1[2], p1[1] * dd + p1[3] * res],
        [-p2[1], p2[2], p2[1] * (dd - lead * ds) + p2[3] * res],
        [
            0.0,
            (total * (e[1] + e[2]) - charge_p) / total,
            (e[0] + total * res * e[3]) / total,
        ],
    ];
    let correction = solve3(jacobian, residual.map(|f| -f)).ok_or_else(|| {
        ComputeError::Invalid("RC effective-capacitance correction is singular".into())
    })?;
    let capacitance = (total + correction[2]).clamp(pi.near, total);
    let (delay, s) = eval(capacitance);
    if !delay.is_finite() || !s.is_finite() || s <= 0.0 {
        return Err(ComputeError::Invalid(
            "invalid NLDM transition for RC fitting".into(),
        ));
    }
    let duration = fit_duration(s, capacitance, res, t)?;
    let slew = (poles.crossing(duration, t.upper) - poles.crossing(duration, t.lower)) / t.derate;
    let root_crossing = poles.crossing(duration, t.output);
    if [slew, duration, root_crossing]
        .iter()
        .any(|v| !v.is_finite())
    {
        return Err(ComputeError::Invalid("RC waveform overflow".into()));
    }
    Ok(GateResponse {
        delay,
        slew,
        capacitance,
        resistance: res,
        duration,
        root_crossing,
    })
}

pub fn gate(
    pi: Pi,
    t: Thresholds,
    eval: impl FnMut(f64) -> (f64, f64),
) -> Result<(f64, f64, f64), ComputeError> {
    let r = gate_detail(pi, t, eval)?;
    Ok((r.delay, r.slew, r.capacitance))
}

/// Exact normalized response of a one-pole network to a unit ramp of duration t.
pub fn ramp_voltage(time: f64, duration: f64, tau: f64) -> f64 {
    if time <= 0.0 {
        return 0.0;
    }
    if tau == 0.0 {
        return if duration == 0.0 {
            1.0
        } else {
            (time / duration).min(1.0)
        };
    }
    if duration == 0.0 {
        return -(-time / tau).exp_m1();
    }
    if time <= duration {
        time / duration * charged_fraction(time, tau)
    } else {
        1.0 + tau / duration * (-duration / tau).exp_m1() * (-(time - duration) / tau).exp()
    }
}

fn crossing_gradient(duration: f64, tau: f64, p: f64, time: f64) -> f64 {
    if tau == 0.0 {
        return p;
    }
    if time <= duration {
        return p / (-(-time / tau).exp_m1());
    }
    let x = duration / tau;
    if x < 1e-3 {
        0.5 + x * (1.0 / 12.0 - x * x / 720.0)
    } else {
        1.0 - 1.0 / x + 1.0 / x.exp_m1()
    }
}

fn crossing(duration: f64, tau: f64, p: f64) -> f64 {
    if tau == 0.0 {
        return p * duration;
    }
    if duration == 0.0 {
        return -tau * (1.0 - p).ln();
    }
    if p >= charged_fraction(duration, tau) {
        return duration - tau * ((1.0 - p) * duration / (-tau * (-duration / tau).exp_m1())).ln();
    }
    if p * duration / tau > 40.0 {
        return p * duration + tau;
    }
    let mut lo = 0.0;
    let mut hi = duration;
    let mut x = (p * duration + tau).min(hi);
    for _ in 0..48 {
        let v = ramp_voltage(x, duration, tau);
        if (v - p).abs() <= 4.0 * f64::EPSILON {
            return x;
        }
        if v < p {
            lo = x;
        } else {
            hi = x;
        }
        let slope = -(-x / tau).exp_m1() / duration;
        let next = x - (v - p) / slope;
        x = if next > lo && next < hi {
            next
        } else {
            lo + (hi - lo) * 0.5
        };
    }
    lo + (hi - lo) * 0.5
}

/// Normalized response at `x` of two cascaded poles `p` and `q` to a unit ramp of duration `d`.
fn cascade(x: f64, d: f64, p: f64, q: f64) -> f64 {
    if (p - q).abs() > 1e-6 * p.max(q) {
        return (p * ramp_voltage(x, d, p) - q * ramp_voltage(x, d, q)) / (p - q);
    }
    let r = 0.5 * (p + q);
    if r == 0.0 {
        return ramp_voltage(x, d, 0.0);
    }
    // Equal poles: the integral of the step response 1 - (1 + t/r) e^(-t/r).
    let integral = |t: f64| {
        if t <= 0.0 {
            0.0
        } else {
            let e = (-t / r).exp();
            t - 2.0 * r * -(-t / r).exp_m1() + t * e
        }
    };
    if d == 0.0 {
        if x <= 0.0 {
            return 0.0;
        }
        return 1.0 - (1.0 + x / r) * (-x / r).exp();
    }
    (integral(x) - integral(x - d)) / d
}

/// The first time at which the monotone response `f` reaches `p` in (0, 1): a doubling bracket
/// from `start` (at most 64 doublings) and at most 100 bisections to a relative 1e-13.
fn monotone_crossing(f: impl Fn(f64) -> f64, p: f64, start: f64) -> Result<f64, ComputeError> {
    let mut hi = start.max(1e-15);
    let mut doublings = 0;
    while f(hi) < p {
        hi *= 2.0;
        doublings += 1;
        if doublings > 64 || !hi.is_finite() {
            return Err(ComputeError::Invalid(
                "RC load crossing not bracketed".into(),
            ));
        }
    }
    let mut lo = 0.0;
    let tolerance = (hi * 1e-13).max(1e-16);
    for _ in 0..100 {
        if hi - lo <= tolerance {
            break;
        }
        let mid = lo + (hi - lo) * 0.5;
        if f(mid) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok(lo + (hi - lo) * 0.5)
}

/// Per-sink threshold delay and transition when a finite-ramp Thevenin driver of resistance
/// `res` (kOhm) and source ramp `duration` (ns) drives the pi `pi`: the sink voltage is the pi
/// root response through a single pole of the sink's first moment `tau`. The delay runs from the
/// root's output trip point to the sink's input trip point; with `tau = 0` the sink is the root.
pub fn wire_from_driver(
    pi: Pi,
    res: f64,
    duration: f64,
    tau: f64,
    t: Thresholds,
) -> Result<(f64, f64), ComputeError> {
    pi.validate()?;
    t.validate()?;
    if !res.is_finite() || res <= 0.0 || pi.resistance <= 0.0 || pi.far <= 0.0 {
        return Err(ComputeError::Invalid(
            "RC driver waveform needs a resistive pi".into(),
        ));
    }
    if !tau.is_finite() || tau < 0.0 || !duration.is_finite() || duration < 0.0 {
        return Err(ComputeError::Invalid("invalid RC wire state".into()));
    }
    let poles = Poles::new(pi, res);
    let w = poles.root_weight;
    let load = |x: f64| {
        w * cascade(x, duration, poles.a, tau) + (1.0 - w) * cascade(x, duration, poles.b, tau)
    };
    let start = duration + poles.a + poles.b + tau;
    let root_out = poles.crossing(duration, t.output);
    let sink_in = monotone_crossing(load, t.input, start)?;
    let slew = (monotone_crossing(load, t.upper, start)?
        - monotone_crossing(load, t.lower, start)?)
        / t.derate;
    let delay = sink_in - root_out;
    if !delay.is_finite() || !slew.is_finite() {
        return Err(ComputeError::Invalid("RC threshold solve overflow".into()));
    }
    Ok((delay, slew.max(0.0)))
}

/// Per-sink threshold delay/transition. Primary ports use an exponential envelope;
/// characterized cell outputs use the finite-ramp convolution exactly.
pub fn wire(tau: f64, slew: f64, t: Thresholds, primary: bool) -> Result<(f64, f64), ComputeError> {
    t.validate()?;
    if !tau.is_finite() || tau < 0.0 || !slew.is_finite() || slew < 0.0 {
        return Err(ComputeError::Invalid("invalid RC wire state".into()));
    }
    let (delay, transition) = if primary {
        (
            -tau * (1.0 - t.input).ln(),
            slew + tau * ((1.0 - t.lower) / (1.0 - t.upper)).ln() / t.derate,
        )
    } else {
        let duration = slew * t.derate / (t.upper - t.lower);
        (
            crossing(duration, tau, t.input) - t.output * duration,
            (crossing(duration, tau, t.upper) - crossing(duration, tau, t.lower)) / t.derate,
        )
    };
    if !delay.is_finite() || !transition.is_finite() {
        return Err(ComputeError::Invalid("RC threshold solve overflow".into()));
    }
    Ok((delay, transition.max(0.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn trips() -> Thresholds {
        Thresholds {
            lower: 0.2,
            upper: 0.8,
            input: 0.5,
            output: 0.5,
            derate: 1.0,
        }
    }
    // STA-REQ-0721: fixed resource bounds, zero resistance and invalid models.
    #[test]
    fn limits_and_degenerate_models() {
        let t = trips();
        let eval = |c: f64| (0.1 + c, 0.3 + c);
        let r = gate_detail(
            Pi {
                near: 0.01,
                far: 0.02,
                resistance: 0.0,
            },
            t,
            eval,
        )
        .unwrap();
        assert_eq!(r.capacitance, 0.03);
        assert!(
            gate_detail(
                Pi {
                    near: -1.0,
                    ..Pi::default()
                },
                t,
                eval
            )
            .is_err()
        );
        assert!(
            gate_detail(
                Pi {
                    near: 0.01,
                    far: 0.02,
                    resistance: 1.0
                },
                t,
                |_| (f64::NAN, 0.1)
            )
            .is_err()
        );
        for tau in [0.0, 1e-20, 0.01, 1e6] {
            let (d, s) = wire(tau, 0.1, t, false).unwrap();
            assert!(d.is_finite() && s.is_finite());
        }
        assert!(wire(1.0, 0.1, Thresholds { derate: 0.0, ..t }, false).is_err());
    }
    // STA-REQ-0726: the caller's resistance reproduces the default estimate bit for bit and
    // keeps its validation; a nonpositive resistance is rejected.
    #[test]
    fn caller_resistance_matches_default_estimate() {
        let t = trips();
        let eval = |c: f64| (0.05 + 2.0 * c, 0.3 + 3.0 * c);
        let pi = Pi {
            near: 0.01,
            far: 0.02,
            resistance: 1.0,
        };
        let a = gate_detail(pi, t, eval).unwrap();
        let b = gate_detail_with_resistance(pi, t, a.resistance, eval).unwrap();
        assert_eq!(a.delay.to_bits(), b.delay.to_bits());
        assert_eq!(a.slew.to_bits(), b.slew.to_bits());
        assert_eq!(a.capacitance.to_bits(), b.capacitance.to_bits());
        assert!(gate_detail_with_resistance(pi, t, 0.0, eval).is_err());
        assert!(gate_detail_with_resistance(pi, t, f64::NAN, eval).is_err());
        let lumped = Pi {
            resistance: 0.0,
            ..pi
        };
        let r = gate_detail_with_resistance(lumped, t, 0.0, eval).unwrap();
        assert_eq!(r.capacitance, 0.03);
    }
    // STA-REQ-0728: the one-step correction is the Newton step of the three residuals (trip
    // points of the single-capacitance response, charge at the end of the source ramp), checked
    // against central differences of the residuals; it lies between the converged charge
    // solution and the total load, and a degenerate pi stays lumped.
    #[test]
    fn one_step_is_the_newton_correction_of_the_driver_residuals() {
        let t = Thresholds {
            lower: 0.3,
            upper: 0.7,
            input: 0.5,
            output: 0.5,
            derate: 0.5,
        };
        let eval = |c: f64| (0.06 + 6.0 * c, 0.09 + 11.0 * c);
        let pi = Pi {
            near: 0.004,
            far: 0.03,
            resistance: 0.8,
        };
        let res = 6.0 * std::f64::consts::LN_2;
        let total = pi.total();
        let span = t.upper - t.lower;
        let lead = t.derate * (t.output - t.lower) / span;
        let poles = Poles::new(pi, res);
        let residual = |x: [f64; 3]| {
            let (d, s) = eval(x[2]);
            let tau = res * x[2];
            [
                ramp_voltage(d - x[0], x[1], tau) - t.output,
                ramp_voltage(d - lead * s - x[0], x[1], tau) - t.lower,
                (x[2] * ramp_voltage(x[1], x[1], tau)
                    - pi.near * poles.root(x[1], x[1])
                    - pi.far * poles.far(x[1], x[1]))
                    / total,
            ]
        };
        let (d0, s0) = eval(total);
        let x0 = [
            d0 - t.output * s0 * t.derate / span,
            s0 * t.derate / span,
            total,
        ];
        let mut jac = [[0.0; 3]; 3];
        for j in 0..3 {
            let h = x0[j] * 1e-6;
            let (mut a, mut b) = (x0, x0);
            a[j] += h;
            b[j] -= h;
            let (fa, fb) = (residual(a), residual(b));
            for i in 0..3 {
                jac[i][j] = (fa[i] - fb[i]) / (2.0 * h);
            }
        }
        let step = solve3(jac, residual(x0).map(|f| -f)).unwrap();
        let r = gate_one_step(pi, t, res, eval).unwrap();
        assert!(
            (r.capacitance - (total + step[2])).abs() < 1e-9 * total,
            "{} {}",
            r.capacitance,
            total + step[2]
        );
        let converged = gate_detail_with_resistance(pi, t, res, eval).unwrap();
        assert!(converged.capacitance < r.capacitance && r.capacitance < total);
        assert_eq!(r.delay, eval(r.capacitance).0);
        let lumped = Pi {
            resistance: 0.0,
            ..pi
        };
        assert_eq!(
            gate_one_step(lumped, t, res, eval).unwrap().capacitance,
            total
        );
        assert!(gate_one_step(pi, t, 0.0, eval).is_err());
        assert!(gate_one_step(pi, t, res, |_| (f64::NAN, 0.1)).is_err());
    }
    // STA-REQ-0729: the sink response is the pi root response convolved with a single pole of
    // the first moment, checked against a direct numerical convolution; with tau = 0 the sink is
    // the root.
    #[test]
    fn wire_follows_the_driver_waveform() {
        let t = trips();
        let pi = Pi {
            near: 0.003,
            far: 0.02,
            resistance: 1.2,
        };
        let res = 4.0;
        let poles = Poles::new(pi, res);
        let duration = 0.21;
        let slew =
            (poles.crossing(duration, t.upper) - poles.crossing(duration, t.lower)) / t.derate;
        let (d, s) = wire_from_driver(pi, res, duration, 0.0, t).unwrap();
        assert!(d.abs() < 1e-12 && (s - slew).abs() < 1e-12, "{d} {s}");
        let tau = 0.035;
        let (d, s) = wire_from_driver(pi, res, duration, tau, t).unwrap();
        // Trapezoidal convolution of the root waveform with exp(-u/tau)/tau on a fine grid.
        let h = 5e-5;
        let n = 20_000;
        let root: Vec<f64> = (0..n).map(|k| poles.root(k as f64 * h, duration)).collect();
        let kernel: Vec<f64> = (0..n)
            .map(|k| (-(k as f64) * h / tau).exp() / tau)
            .collect();
        let sink = |m: usize| {
            let mut acc = 0.0;
            for k in 0..=m {
                let w = if k == 0 || k == m { 0.5 } else { 1.0 };
                acc += w * root[m - k] * kernel[k];
            }
            acc * h
        };
        let cross = |p: f64| {
            let mut prev = 0.0;
            for m in 1..n {
                let v = sink(m);
                if v >= p {
                    return (m as f64 - 1.0 + (p - prev) / (v - prev)) * h;
                }
                prev = v;
            }
            f64::NAN
        };
        let delay = cross(t.input) - poles.crossing(duration, t.output);
        let transition = (cross(t.upper) - cross(t.lower)) / t.derate;
        assert!((delay - d).abs() < 2e-6, "{delay} {d}");
        assert!((transition - s).abs() < 2e-6, "{transition} {s}");
        assert!(d > 0.0 && s > slew);
        assert!(
            wire_from_driver(
                Pi {
                    resistance: 0.0,
                    ..pi
                },
                res,
                duration,
                tau,
                t
            )
            .is_err()
        );
        assert!(wire_from_driver(pi, res, -1.0, tau, t).is_err());
    }
    // STA-REQ-0722: normalized polarity/derate, unchanged zero RC, stable small arguments.
    #[test]
    fn stable_charge_and_zero_rc() {
        assert!((charged_fraction(1e-10, 1.0) - 5e-11).abs() < 1e-20);
        let t = trips();
        let (d, s) = wire(0.0, 0.2, t, false).unwrap();
        assert_eq!(d, 0.0);
        assert!((s - 0.2).abs() < 1e-15);
        let (d, s) = wire(0.01, 0.1, t, true).unwrap();
        assert!((d - 0.01 * 2.0_f64.ln()).abs() < 1e-16);
        assert!(s > 0.1);
    }
}
