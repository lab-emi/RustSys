// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! External nodal transient validation, not a second RC tree implementation.
//! Test peak estimate: six sequential four-node circuits, <32 MiB each; no paths.
use rusteda_compute::rc::{self, Pi, Thresholds};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    process::{Command, Stdio},
};

fn measurements(text: &str) -> BTreeMap<String, f64> {
    text.lines()
        .filter_map(|l| {
            let (key, value) = l.split_once('=')?;
            Some((
                key.trim().to_string(),
                value.split_whitespace().next()?.parse().ok()?,
            ))
        })
        .collect()
}

// STA-REQ-0724: the external simulator independently solves capacitor current ODEs.
#[test]
#[ignore = "requires RUSTEDA_SPICE_SIMULATOR and RUSTEDA_RC_ORACLE_WORKSPACE, with OS caps"]
fn simulator_charge_and_thresholds() {
    let tool = PathBuf::from(
        std::env::var("RUSTEDA_SPICE_SIMULATOR").expect("external simulator required"),
    );
    let root = PathBuf::from(
        std::env::var("RUSTEDA_RC_ORACLE_WORKSPACE").expect("evidence workspace required"),
    );
    fs::create_dir(&root).unwrap();
    for (i, (res, derate, low, high)) in [
        (0.01, 1.0, 0.2, 0.8),
        (1.0, 1.0, 0.2, 0.8),
        (20.0, 1.0, 0.2, 0.8),
        (0.1, 0.5, 0.3, 0.7),
        (5.0, 0.5, 0.3, 0.7),
    ]
    .into_iter()
    .enumerate()
    {
        let pi = Pi {
            near: 0.01,
            far: 0.02,
            resistance: res,
        };
        let t = Thresholds {
            lower: low,
            upper: high,
            input: 0.5,
            output: 0.5,
            derate,
        };
        // A bounded synthetic NLDM law; the oracle receives the fitted circuit,
        // then checks its actual charge and measured voltage crossing times.
        let lookup = |c: f64| (0.05 + 2.0 * c, 0.3 + 3.0 * c);
        let r = rc::gate_detail(pi, t, lookup).unwrap();
        let stop = 0.1 + r.duration + 30.0 * (r.resistance * pi.total() + pi.resistance * pi.far);
        let step = stop / 100000.0;
        let path = root.join(format!("case{i}.cir"));
        let log = root.join(format!("case{i}.log"));
        let circuit = format!(
            "RC driver transient\nVsrc src 0 PWL(0 0 0.1n 0 {end}n 1)\nRd src root {rd}\nRpi root far {rp}\nCn root 0 {cn}p\nCf far 0 {cf}p\nRnom src nom {rd}\nCnom nom 0 {ce}p\n.control\nset numdgt=17\ntran {step}n {stop}n\nmeas tran tlo WHEN v(root)={low} RISE=1\nmeas tran tmid WHEN v(root)=0.5 RISE=1\nmeas tran thi WHEN v(root)={high} RISE=1\nmeas tran ntlo WHEN v(nom)={low} RISE=1\nmeas tran ntmid WHEN v(nom)=0.5 RISE=1\nmeas tran vr FIND v(root) AT={end}n\nmeas tran vf FIND v(far) AT={end}n\nmeas tran vn FIND v(nom) AT={end}n\nquit\n.endc\n.end\n",
            end = 0.1 + r.duration,
            rd = r.resistance * 1000.0,
            rp = pi.resistance * 1000.0,
            cn = pi.near,
            cf = pi.far,
            ce = r.capacitance
        );
        fs::write(&path, circuit).unwrap();
        let file = fs::File::create(&log).unwrap();
        let status = Command::new("timeout")
            .args(["--kill-after=2s", "35s"])
            .arg(&tool)
            .args(["-b"])
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::from(file.try_clone().unwrap()))
            .stderr(Stdio::from(file))
            .status()
            .unwrap();
        assert!(status.success(), "simulator failed: {}", log.display());
        assert!(fs::metadata(&log).unwrap().len() < 1 << 20);
        let m = measurements(&fs::read_to_string(log).unwrap());
        let crossing = (m["tmid"] * 1e9 - 0.1 - r.root_crossing).abs();
        let slew = ((m["thi"] - m["tlo"]) * 1e9 / derate - r.slew).abs();
        let fit = ((m["ntmid"] - m["ntlo"]) * 1e9
            - lookup(r.capacitance).1 * derate * (0.5 - low) / (high - low))
            .abs();
        let charge = (pi.near * m["vr"] + pi.far * m["vf"] - r.capacitance * m["vn"]).abs();
        fs::write(root.join(format!("case{i}.metrics")),format!("crossing_error_ns {crossing:e}\nslew_error_ns {slew:e}\nfit_error_ns {fit:e}\ncharge_error_pf {charge:e}\n")).unwrap();
        assert!(crossing < 1e-6 && slew < 1e-6 && fit < 1e-6 && charge < 1e-8);
    }
}

// STA-REQ-0722: ideal-source single-pole step/ramp response, independent transient oracle.
#[test]
#[ignore = "requires RUSTEDA_SPICE_SIMULATOR and a fresh RUSTEDA_RC_WIRE_WORKSPACE, with OS caps"]
fn simulator_wire_thresholds() {
    let tool = std::env::var("RUSTEDA_SPICE_SIMULATOR").expect("external simulator required");
    let root = PathBuf::from(
        std::env::var("RUSTEDA_RC_WIRE_WORKSPACE").expect("evidence workspace required"),
    );
    fs::create_dir(&root).unwrap();
    for (i, (tau, s, primary, derate, low, high)) in [
        (0.02, 0.0, true, 1.0, 0.2, 0.8),
        (0.002, 0.3, false, 1.0, 0.2, 0.8),
        (0.1, 0.02, false, 1.0, 0.2, 0.8),
        (0.2, 0.3, false, 0.5, 0.3, 0.7),
    ]
    .into_iter()
    .enumerate()
    {
        let t = Thresholds {
            lower: low,
            upper: high,
            input: 0.5,
            output: 0.5,
            derate,
        };
        let (d, q) = rc::wire(tau, s, t, primary).unwrap();
        let duration = s * derate / (high - low);
        let end = 0.1 + duration.max(1e-8);
        let stop = end + 30.0 * tau;
        let path = root.join(format!("wire{i}.cir"));
        let log = root.join(format!("wire{i}.log"));
        fs::write(&path,format!("RC interconnect transient\nVsrc in 0 PWL(0 0 0.1n 0 {end}n 1)\nR1 in out {r}\nC1 out 0 1p\n.control\nset numdgt=17\ntran {step}n {stop}n\nmeas tran tlo WHEN v(out)={low} RISE=1\nmeas tran tmid WHEN v(out)=0.5 RISE=1\nmeas tran thi WHEN v(out)={high} RISE=1\nquit\n.endc\n.end\n",r=tau*1000.0,step=stop/100000.0)).unwrap();
        let file = fs::File::create(&log).unwrap();
        let status = Command::new("timeout")
            .args(["--kill-after=2s", "35s", &tool, "-b"])
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::from(file.try_clone().unwrap()))
            .stderr(Stdio::from(file))
            .status()
            .unwrap();
        assert!(status.success());
        assert!(fs::metadata(&log).unwrap().len() < 1 << 20);
        let m = measurements(&fs::read_to_string(log).unwrap());
        let de = (m["tmid"] * 1e9 - 0.1 - duration * 0.5 - d).abs();
        let se = ((m["thi"] - m["tlo"]) * 1e9 / derate - q).abs();
        fs::write(
            root.join(format!("wire{i}.metrics")),
            format!("delay_error_ns {de:e}\nslew_error_ns {se:e}\n"),
        )
        .unwrap();
        assert!(de < 1e-6 && se < 1e-6);
    }
}
