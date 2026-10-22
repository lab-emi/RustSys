// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! A bounded search changes one carry chain at a time; a trial never changes later trials.

use crate::SynthError;
use rusteda_core::job::JobError;

pub(super) fn run<T>(
    widths: &[usize],
    mut evaluate: impl FnMut(&[usize]) -> Result<T, JobError<SynthError>>,
    complete: impl Fn(&T) -> bool,
) -> Result<Option<T>, JobError<SynthError>> {
    if widths.len() > 64 {
        return Ok(None);
    }
    // Reverse lowering order considers downstream arithmetic before earlier producers.
    for (i, &width) in widths
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, w)| **w >= 8)
        .take(12)
    {
        let mut plan = vec![0; widths.len()];
        plan[i] = width;
        match evaluate(&plan) {
            Ok(candidate) if complete(&candidate) => return Ok(Some(candidate)),
            Err(JobError::Cancelled) => return Err(JobError::Cancelled),
            _ => {}
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    // SYNTH-REQ-0560: neither area alone nor timing alone is enough, and plans do not accumulate.
    #[test]
    fn returns_only_a_jointly_feasible_individual_change() {
        let mut plans = Vec::new();
        let result = run(
            &[8, 16, 0, 32],
            |plan| {
                plans.push(plan.to_vec());
                let quality = if plan[3] != 0 {
                    (false, true, true)
                } else if plan[1] != 0 {
                    (true, false, true)
                } else {
                    (true, true, true)
                };
                Ok((plan.to_vec(), quality))
            },
            |(_, q)| q.0 && q.1 && q.2,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.0, [8, 0, 0, 0]);
        assert_eq!(
            plans,
            [vec![0, 0, 0, 32], vec![0, 16, 0, 0], vec![8, 0, 0, 0]]
        );
    }

    #[test]
    fn cancellation_is_not_an_unusable_implementation() {
        let r = run::<()>(&[32], |_| Err(JobError::Cancelled), |_| true);
        assert!(matches!(r, Err(JobError::Cancelled)));
    }
}
