//! Gain check (the Level meter's control on the Main tab, or `g`): listen to
//! the meter for `CHECK_SECS` while the user speaks, then suggest a Manual
//! gain that puts the loudest peak near `TARGET_PEAK_X10`. The Main tab's
//! right column shows it as a card. It only reads: the user applies the
//! suggestion with the arrow keys, so `apply_action()` stays the only writer.
//!
//! The suggestion assumes a gain change moves the peak by the same number of
//! dB. That is unconfirmed on hardware, and the compressor and limiter break
//! it, so the result reads "try", and a second check shows how close it got.

use std::time::{Duration, Instant};

use crate::protocol::DeviceModel;

/// How long the check listens.
pub const CHECK_SECS: u64 = 10;

/// Peaks before this are left out: the meter's short window still holds up to
/// 0.3 s from before the check, and the key that started it may click into
/// the mic.
const SETTLE: Duration = Duration::from_millis(500);

/// The loudest peak to aim for, in dBFS × 10 (as the meter stores it): 12 dB
/// of headroom for laughter or leaning in, and where common podcast advice
/// for raw speech puts it.
pub const TARGET_PEAK_X10: i32 = -120;

/// How far, in dBFS × 10, the loudest peak may be from the target and still
/// "look right", so nobody is told to move a dB or two.
const GOOD_BAND_X10: i32 = 30;

/// A loudest peak at or above this, in dBFS × 10, is called clipping.
const CLIP_X10: i32 = -10;

/// Below this, in dBFS × 10, the check heard nothing it can use: the meter
/// shows -60 dBFS at its floor.
const SILENT_X10: i32 = -500;

/// A gain check: listening, or its result, shown until it is closed.
pub enum GainCheck {
    Listening(Listening),
    Done(Outcome),
}

/// A check in progress.
pub struct Listening {
    /// When sampling began: the first `step()` after the check started.
    /// `None` until then.
    started: Option<Instant>,
    /// The gain the check started at. A change starts it over, since the
    /// loudest peak would otherwise mix two gains.
    pub gain_tenths: u16,
    /// The loudest peak heard so far, in dBFS × 10.
    pub loudest_x10: Option<i32>,
}

/// What a finished check heard, and the gain it suggests.
pub struct Outcome {
    /// The loudest peak, in dBFS × 10; `None` when nothing came in at all.
    pub loudest_x10: Option<i32>,
    pub advice: Advice,
}

/// What a check found, before it is put in words.
#[derive(Debug, PartialEq)]
pub enum Advice {
    /// Nothing above `SILENT_X10`.
    NoSignal,
    /// The loudest peak is within `GOOD_BAND_X10` of the target.
    Good,
    /// Try this gain, in tenths of a dB.
    Change(u16),
    /// The gain needed is past the model's range: already at its maximum
    /// (`true`) or at 0 dB (`false`).
    AtLimit { raise: bool },
}

impl Listening {
    pub fn new(gain_tenths: u16) -> Self {
        Self {
            started: None,
            gain_tenths,
            loudest_x10: None,
        }
    }

    /// Whole seconds left at `now`, rounded up, for the countdown.
    pub fn seconds_left(&self, now: Instant) -> u64 {
        let elapsed = self.started.map_or(Duration::ZERO, |started| {
            now.saturating_duration_since(started)
        });
        Duration::from_secs(CHECK_SECS)
            .saturating_sub(elapsed)
            .as_millis()
            .div_ceil(1000) as u64
    }

    /// Take the meter's current short-window peak (`None` when it holds
    /// nothing recent), and return the outcome once `CHECK_SECS` have passed.
    pub fn sample(
        &mut self,
        now: Instant,
        peak_x10: Option<i32>,
        model: DeviceModel,
    ) -> Option<Outcome> {
        let started = *self.started.get_or_insert(now);
        let elapsed = now.saturating_duration_since(started);
        if elapsed >= SETTLE
            && let Some(peak) = peak_x10
        {
            self.loudest_x10 = Some(self.loudest_x10.map_or(peak, |loudest| loudest.max(peak)));
        }
        (elapsed >= Duration::from_secs(CHECK_SECS)).then(|| Outcome {
            loudest_x10: self.loudest_x10,
            advice: advise(model, self.gain_tenths, self.loudest_x10),
        })
    }
}

impl Outcome {
    /// Whether the loudest peak reached full scale.
    pub fn clipping(&self) -> bool {
        self.loudest_x10.is_some_and(|loudest| loudest >= CLIP_X10)
    }
}

/// The gain to try for a loudest peak of `loudest_x10` at `gain_tenths`.
pub fn advise(model: DeviceModel, gain_tenths: u16, loudest_x10: Option<i32>) -> Advice {
    let Some(loudest) = loudest_x10.filter(|loudest| *loudest >= SILENT_X10) else {
        return Advice::NoSignal;
    };
    let change = TARGET_PEAK_X10 - loudest;
    if change.abs() <= GOOD_BAND_X10 {
        return Advice::Good;
    }
    // Clamped to the model's range and rounded down onto its step grid, as
    // every gain write is: down is the safe side when raising.
    let wanted = (i32::from(gain_tenths) + change).max(0);
    let suggested = model.snap_gain_tenths(u16::try_from(wanted).unwrap_or(u16::MAX));
    if suggested == gain_tenths {
        Advice::AtLimit { raise: change > 0 }
    } else {
        Advice::Change(suggested)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_peak_near_the_target_looks_right() {
        for loudest in [-150, -120, -90] {
            assert_eq!(advise(DeviceModel::Mv7, 240, Some(loudest)), Advice::Good);
        }
    }

    #[test]
    fn a_quiet_or_loud_peak_moves_the_gain_toward_the_target() {
        // 24 dB with peaks at -22.4 dBFS: 10.4 dB up, rounded down to the step.
        assert_eq!(
            advise(DeviceModel::Mv6, 240, Some(-224)),
            Advice::Change(340)
        );
        assert_eq!(
            advise(DeviceModel::Mv7, 240, Some(-224)),
            Advice::Change(330)
        );
        // Peaks at -4 dBFS: 8 dB down.
        assert_eq!(
            advise(DeviceModel::Mv7Plus, 240, Some(-40)),
            Advice::Change(160)
        );
    }

    #[test]
    fn the_suggestion_stays_in_the_models_range() {
        // 30 dB up from 30 dB: past the MV6's 36 dB, within the MVX2U's 60 dB.
        assert_eq!(
            advise(DeviceModel::Mv6, 300, Some(-420)),
            Advice::Change(360)
        );
        assert_eq!(
            advise(DeviceModel::Mvx2u, 300, Some(-420)),
            Advice::Change(600)
        );
        assert_eq!(advise(DeviceModel::Mv6, 50, Some(0)), Advice::Change(0));
    }

    #[test]
    fn a_gain_already_at_its_limit_says_to_move() {
        assert_eq!(
            advise(DeviceModel::Mv6, 360, Some(-300)),
            Advice::AtLimit { raise: true }
        );
        assert_eq!(
            advise(DeviceModel::Mvx2u, 0, Some(-20)),
            Advice::AtLimit { raise: false }
        );
    }

    #[test]
    fn silence_is_no_signal() {
        assert_eq!(advise(DeviceModel::Mv6, 240, None), Advice::NoSignal);
        assert_eq!(advise(DeviceModel::Mv6, 240, Some(-510)), Advice::NoSignal);
    }

    #[test]
    fn the_check_skips_the_settle_time_and_ends_after_check_secs() {
        let start = Instant::now();
        let mut check = Listening::new(240);
        assert_eq!(check.seconds_left(start), CHECK_SECS);
        let model = DeviceModel::Mv6;
        // A loud click while settling is left out.
        assert!(check.sample(start, Some(0), model).is_none());
        let at = |ms| start + Duration::from_millis(ms);
        assert!(check.sample(at(300), Some(0), model).is_none());
        assert!(check.sample(at(600), Some(-224), model).is_none());
        assert_eq!(check.seconds_left(at(600)), 10);
        assert_eq!(check.seconds_left(at(9_100)), 1);
        let outcome = check.sample(at(10_000), None, model).expect("done");
        assert_eq!(outcome.loudest_x10, Some(-224));
        assert_eq!(outcome.advice, Advice::Change(340));
        assert!(!outcome.clipping());
    }
}
