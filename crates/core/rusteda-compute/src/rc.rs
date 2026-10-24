// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Bounded f64 ramp/charge models, independently derived from RC circuit equations.
//! STA-REQ-0721, STA-REQ-0722. All times ns, capacitances pF, resistances kOhm.
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
