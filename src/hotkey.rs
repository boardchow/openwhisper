//! Watches the keyboard with a CGEventTap on the main run loop:
//! - a quick standalone tap of the shortcut key (right ⌘ by default) toggles
//!   dictation; using the key as a modifier in a shortcut does not count, so it
//!   keeps working normally;
//! - Esc while dictating cancels, and is swallowed so the focused app doesn't
//!   also act on it (Esc interrupts Claude Code, closes dialogs…).
//!
//! Needs Accessibility permission.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
use core_graphics::event::{
    CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType, CallbackResult, EventField,
};

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// Modifier keys that can be the shortcut: (macOS keycode, menu title).
pub const KEYS: [(i64, &str); 4] = [(54, "Right ⌘"), (61, "Right ⌥"), (62, "Right ⌃"), (63, "fn")];
const ESCAPE: i64 = 53;
const TAP_MAX: Duration = Duration::from_millis(500);

/// The shortcut key's keycode; the menu changes it at runtime.
pub static KEY: AtomicI64 = AtomicI64::new(54);
/// Set on each clean tap of the shortcut key.
pub static TAPPED: AtomicBool = AtomicBool::new(false);
/// Set when Esc is pressed while dictating.
pub static CANCELLED: AtomicBool = AtomicBool::new(false);
/// Whether dictation is in progress, so Esc is ours to take.
pub static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Installs the tap. Returns None when Accessibility has not been granted yet.
pub fn install() -> Option<CGEventTap<'static>> {
    // Untrusted processes still get a tap back; it just never receives events.
    if !unsafe { AXIsProcessTrusted() } {
        return None;
    }
    let keys = Cell::new(Keys::default());
    let tap = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default, // not ListenOnly: Esc must be droppable
        // Clicks and scrolls are watched too, so ⌘-click or ⌘-scroll doesn't count as a tap.
        vec![
            CGEventType::FlagsChanged,
            CGEventType::KeyDown,
            CGEventType::LeftMouseDown,
            CGEventType::RightMouseDown,
            CGEventType::OtherMouseDown,
            CGEventType::ScrollWheel,
        ],
        move |_, kind, event| {
            let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
            let mut k = keys.get();
            let action = k.handle(
                matches!(kind, CGEventType::KeyDown),
                keycode,
                KEY.load(Ordering::Relaxed),
                ACTIVE.load(Ordering::SeqCst),
                Instant::now(),
            );
            keys.set(k);
            match action {
                Action::Pass => CallbackResult::Keep,
                Action::Tapped => {
                    TAPPED.store(true, Ordering::SeqCst);
                    CallbackResult::Keep
                }
                Action::Cancel => {
                    CANCELLED.store(true, Ordering::SeqCst);
                    CallbackResult::Drop
                }
            }
        },
    )
    .ok()?;
    let source = tap.mach_port().create_runloop_source(0).ok()?;
    CFRunLoop::get_main().add_source(&source, unsafe { kCFRunLoopCommonModes });
    tap.enable();
    Some(tap)
}

#[derive(Debug, PartialEq)]
enum Action {
    /// Let the event through.
    Pass,
    /// A clean tap of the shortcut key.
    Tapped,
    /// Esc while dictating: cancel, and swallow the event.
    Cancel,
}

/// Tells a quick standalone tap of the shortcut key from its use as a modifier.
#[derive(Clone, Copy, Default)]
struct Keys {
    down_at: Option<Instant>,
    used_as_modifier: bool,
}

impl Keys {
    /// Feeds one event: a key press (`key_down`) or a modifier change, with its keycode.
    fn handle(&mut self, key_down: bool, keycode: i64, shortcut: i64, active: bool, now: Instant) -> Action {
        if key_down && keycode == ESCAPE && active {
            return Action::Cancel;
        }
        if !key_down && keycode == shortcut {
            return match self.down_at.take() {
                None => {
                    self.down_at = Some(now);
                    self.used_as_modifier = false;
                    Action::Pass
                }
                Some(t) if !self.used_as_modifier && now - t < TAP_MAX => Action::Tapped,
                Some(_) => Action::Pass,
            };
        }
        // Any other key or modifier while the shortcut key is held makes it a shortcut.
        if self.down_at.is_some() {
            self.used_as_modifier = true;
        }
        Action::Pass
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: i64 = 54;
    const C_KEY: i64 = 8;
    const SHIFT: i64 = 56;

    /// Plays (key_down, keycode, ms since start) events; returns the actions.
    fn play(active: bool, events: &[(bool, i64, u64)]) -> Vec<Action> {
        let (mut keys, start) = (Keys::default(), Instant::now());
        events
            .iter()
            .map(|&(down, code, ms)| keys.handle(down, code, CMD, active, start + Duration::from_millis(ms)))
            .collect()
    }

    #[test]
    fn a_quick_standalone_tap_fires() {
        assert_eq!(
            play(false, &[(false, CMD, 0), (false, CMD, 120)]),
            [Action::Pass, Action::Tapped]
        );
    }

    #[test]
    fn holding_too_long_does_not_fire() {
        assert_eq!(
            play(false, &[(false, CMD, 0), (false, CMD, 800)]),
            [Action::Pass, Action::Pass]
        );
    }

    #[test]
    fn using_the_key_in_a_shortcut_does_not_fire() {
        // ⌘C
        assert_eq!(
            play(false, &[(false, CMD, 0), (true, C_KEY, 50), (false, CMD, 100)])[2],
            Action::Pass
        );
        // ⌘⇧ with another modifier
        assert_eq!(
            play(
                false,
                &[
                    (false, CMD, 0),
                    (false, SHIFT, 30),
                    (false, SHIFT, 60),
                    (false, CMD, 90)
                ]
            )[3],
            Action::Pass
        );
        // The next clean tap works again.
        let actions = play(
            false,
            &[
                (false, CMD, 0),
                (true, C_KEY, 50),
                (false, CMD, 100),
                (false, CMD, 200),
                (false, CMD, 300),
            ],
        );
        assert_eq!(actions[4], Action::Tapped);
    }

    #[test]
    fn a_click_while_held_makes_it_a_modifier() {
        // Mouse events carry no keycode (0) and aren't key presses.
        assert_eq!(
            play(false, &[(false, CMD, 0), (false, 0, 50), (false, CMD, 100)])[2],
            Action::Pass
        );
    }

    #[test]
    fn typing_without_the_shortcut_held_is_ignored() {
        assert_eq!(
            play(false, &[(true, C_KEY, 0), (false, SHIFT, 10)]),
            [Action::Pass, Action::Pass]
        );
    }

    #[test]
    fn escape_cancels_only_while_dictating() {
        assert_eq!(play(true, &[(true, ESCAPE, 0)]), [Action::Cancel]);
        assert_eq!(play(false, &[(true, ESCAPE, 0)]), [Action::Pass]);
    }
}
