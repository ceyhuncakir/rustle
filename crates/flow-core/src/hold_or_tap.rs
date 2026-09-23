//! One shortcut, two behaviours.
//!
//! Which one you get is decided by how long the key stays down after the
//! shortcut fires: released quickly means a tap that keeps recording until the
//! next tap; held then released means push-to-talk, which stops the moment
//! the key comes up. Deciding at press time cannot work - the key is always
//! still down then, so every press would look like a hold and the tap mode
//! could never be reached.
//!
//! This is the exact logic of the GNOME extension, lifted into the core so
//! every platform that reports raw key state behaves identically.

use std::time::{Duration, Instant};

/// Holding the shortcut for at least this long means push-to-talk; anything
/// shorter is a tap that latches recording on until the next tap.
const HOLD_THRESHOLD: Duration = Duration::from_millis(350);
/// A shortcut that fires 30 times a second thrashes the engine badly enough
/// to be worth a second line of defence beyond ignoring key auto-repeat.
const DEBOUNCE: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
}

#[derive(Debug)]
pub struct HoldOrTap {
    push_to_talk: bool,
    recording: bool,
    pressed_at: Option<Instant>,
    last_fire: Option<Instant>,
}

impl HoldOrTap {
    pub fn new(push_to_talk: bool) -> Self {
        Self { push_to_talk, recording: false, pressed_at: None, last_fire: None }
    }

    pub fn on_down(&mut self, now: Instant) -> Option<Action> {
        if self.last_fire.is_some_and(|last| now.duration_since(last) < DEBOUNCE) {
            return None;
        }
        self.last_fire = Some(now);

        if self.recording {
            self.recording = false;
            self.pressed_at = None;
            return Some(Action::Stop);
        }

        self.recording = true;
        self.pressed_at = if self.push_to_talk { Some(now) } else { None };
        Some(Action::Start)
    }

    pub fn on_up(&mut self, now: Instant) -> Option<Action> {
        let pressed_at = self.pressed_at.take()?;
        if self.recording && now.duration_since(pressed_at) >= HOLD_THRESHOLD {
            self.recording = false;
            return Some(Action::Stop);
        }
        // A tap: leave it recording and wait for the next press.
        None
    }

    /// Forget any in-flight press, e.g. after a cancel or an error.
    pub fn reset(&mut self) {
        self.recording = false;
        self.pressed_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn hold_is_push_to_talk() {
        let mut h = HoldOrTap::new(true);
        let start = t0();
        assert_eq!(h.on_down(start), Some(Action::Start));
        assert_eq!(h.on_up(start + Duration::from_millis(400)), Some(Action::Stop));
    }

    #[test]
    fn tap_latches_until_the_next_tap() {
        let mut h = HoldOrTap::new(true);
        let start = t0();
        assert_eq!(h.on_down(start), Some(Action::Start));
        assert_eq!(h.on_up(start + Duration::from_millis(100)), None);
        assert_eq!(h.on_down(start + Duration::from_millis(2000)), Some(Action::Stop));
        // The release of the stopping tap does nothing.
        assert_eq!(h.on_up(start + Duration::from_millis(2100)), None);
    }

    #[test]
    fn exactly_at_threshold_counts_as_hold() {
        let mut h = HoldOrTap::new(true);
        let start = t0();
        h.on_down(start);
        assert_eq!(h.on_up(start + HOLD_THRESHOLD), Some(Action::Stop));
    }

    #[test]
    fn repeats_inside_the_debounce_are_ignored() {
        let mut h = HoldOrTap::new(true);
        let start = t0();
        assert_eq!(h.on_down(start), Some(Action::Start));
        assert_eq!(h.on_down(start + Duration::from_millis(30)), None);
        assert_eq!(h.on_down(start + Duration::from_millis(60)), None);
        assert_eq!(h.on_down(start + Duration::from_millis(200)), Some(Action::Stop));
    }

    #[test]
    fn push_to_talk_off_means_toggle_only() {
        let mut h = HoldOrTap::new(false);
        let start = t0();
        assert_eq!(h.on_down(start), Some(Action::Start));
        assert_eq!(h.on_up(start + Duration::from_secs(5)), None);
        assert_eq!(h.on_down(start + Duration::from_secs(6)), Some(Action::Stop));
    }

    #[test]
    fn reset_forgets_the_press() {
        let mut h = HoldOrTap::new(true);
        let start = t0();
        h.on_down(start);
        h.reset();
        assert_eq!(h.on_up(start + Duration::from_secs(1)), None);
        assert_eq!(h.on_down(start + Duration::from_secs(2)), Some(Action::Start));
    }
}
