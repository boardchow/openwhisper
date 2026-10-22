//! The recording pill: a lime capsule, no border, with seven
//! black bars, floating just above the Dock. It's a non-activating,
//! click-through panel, so it never steals focus from the field being typed in.
//! Geometry and level mapping follow Wispr Flow's overlay; the colour is ours.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSPanel, NSScreen, NSScreenSaverWindowLevel, NSTitlePosition,
    NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

const WIDTH: f64 = 60.0;
const HEIGHT: f64 = 30.0;
const BOTTOM_MARGIN: f64 = 14.0;
const BARS: usize = 7;
const BAR_WIDTH: f64 = 2.0;
const BAR_GAP: f64 = 2.0;
const BAR_MIN: f64 = 2.0;
const BAR_MAX: f64 = 18.0;
const NOISE_FLOOR_MIN_DB: f64 = -60.0;
// Level above which bars turn from dimmed to solid ("voice detected").
const VOICE: f64 = 0.15;
// Lime, #BFF549.
const FILL: (f64, f64, f64) = (0.749, 0.961, 0.286);
// Black bars, at this alpha while quiet and solid on voice.
const DIM: f64 = 0.45;
// One bar's 1 s "breathing" cycle: (time, height multiplier).
const BREATH: [(f64, f64); 6] = [(0.0, 1.0), (0.2, 1.2), (0.4, 1.5), (0.8, 1.1), (0.9, 1.3), (1.0, 1.0)];

pub struct Pill {
    panel: Retained<NSPanel>,
    bars: Vec<Retained<NSBox>>,
    heights: RefCell<[f64; BARS]>,
    level: Cell<f64>,
    floor_db: Cell<f64>,
}

impl Pill {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setLevel(NSScreenSaverWindowLevel);
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setIgnoresMouseEvents(true);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary,
        );

        let root = NSView::initWithFrame(NSView::alloc(mtm), frame);
        let fill = NSColor::colorWithSRGBRed_green_blue_alpha(FILL.0, FILL.1, FILL.2, 1.0);
        let background = rounded_box(mtm, frame, HEIGHT / 2.0, &fill);
        root.addSubview(&background);

        let bars: Vec<_> = (0..BARS)
            .map(|_| {
                let bar = rounded_box(mtm, frame, 0.5, &NSColor::blackColor());
                root.addSubview(&bar);
                bar
            })
            .collect();
        panel.setContentView(Some(&root));

        let pill = Pill {
            panel,
            bars,
            heights: RefCell::new([BAR_MIN; BARS]),
            level: Cell::new(0.0),
            floor_db: Cell::new(0.0),
        };
        pill.layout(1.0);
        pill
    }

    /// Shows the pill bottom-center on the screen that has the focused window.
    pub fn show(&self) {
        let mtm = MainThreadMarker::from(&*self.panel);
        if let Some(screen) = NSScreen::mainScreen(mtm) {
            let area = screen.visibleFrame();
            let x = area.origin.x + (area.size.width - WIDTH) / 2.0;
            self.panel
                .setFrameOrigin(NSPoint::new(x.round(), area.origin.y + BOTTOM_MARGIN));
        }
        *self.heights.borrow_mut() = [BAR_MIN; BARS];
        self.level.set(0.0);
        self.floor_db.set(0.0);
        self.layout(DIM);
        self.panel.orderFrontRegardless();
    }

    pub fn hide(&self) {
        self.panel.orderOut(None);
    }

    /// Bars follow the mic level measured above the room's noise floor, each
    /// breathing on its own staggered cycle so they don't move in lockstep.
    pub fn draw_level(&self, rms: f32, t: f64) {
        let (level, floor) = smooth_level(rms, self.level.get(), self.floor_db.get());
        self.level.set(level);
        self.floor_db.set(floor);
        *self.heights.borrow_mut() = level_heights(level, t);
        self.layout(if level > VOICE { 1.0 } else { DIM });
    }

    /// A small wave travelling across the bars while whisper finishes.
    pub fn draw_thinking(&self, t: f64) {
        *self.heights.borrow_mut() = thinking_heights(t);
        self.layout(DIM);
    }

    fn layout(&self, alpha: f64) {
        let color = NSColor::colorWithWhite_alpha(0.0, alpha);
        let total = BARS as f64 * (BAR_WIDTH + BAR_GAP) - BAR_GAP;
        let left = (WIDTH - total) / 2.0;
        for (i, (bar, h)) in self.bars.iter().zip(self.heights.borrow().iter()).enumerate() {
            let x = left + i as f64 * (BAR_WIDTH + BAR_GAP);
            bar.setFrame(NSRect::new(
                NSPoint::new(x, (HEIGHT - h) / 2.0),
                NSSize::new(BAR_WIDTH, *h),
            ));
            bar.setFillColor(&color);
        }
    }
}

/// One meter step: the new 0–1 level for this mic RMS, measured above the
/// room's noise floor (the quietest level heard so far), and the new floor in dB.
fn smooth_level(rms: f32, level: f64, floor_db: f64) -> (f64, f64) {
    let db = 20.0 * (rms as f64).max(1e-6).log10();
    let floor = floor_db.min(db).max(NOISE_FLOOR_MIN_DB);
    (0.85 * level + 0.15 * ((db - floor) / 20.0).clamp(0.0, 1.0), floor)
}

/// Bar heights for a 0–1 level at time `t` (seconds).
fn level_heights(level: f64, t: f64) -> [f64; BARS] {
    std::array::from_fn(|i| {
        // Tallest in the middle, down to 35% at the edges, whatever the bar count.
        let d = (i as f64 - (BARS as f64 - 1.0) / 2.0) / ((BARS as f64 - 1.0) / 2.0);
        let shape = 1.0 - 0.65 * d * d;
        // Staggered phases make a ripple run across the bars.
        (BAR_MIN * (5.0 * level).max(1.0) * shape * breath(t - i as f64 * 0.15)).clamp(BAR_MIN, BAR_MAX)
    })
}

fn thinking_heights(t: f64) -> [f64; BARS] {
    std::array::from_fn(|i| BAR_MIN + 4.0 * (0.5 + 0.5 * (t * 8.0 - i as f64 * 0.6).sin()))
}

/// The breathing multiplier at time `t` (seconds), eased between keyframes.
fn breath(t: f64) -> f64 {
    let p = t.rem_euclid(1.0);
    let k = BREATH.windows(2).find(|w| p <= w[1].0).unwrap_or(&BREATH[4..]);
    let ((t0, v0), (t1, v1)) = (k[0], k[1]);
    let x = if t1 > t0 { (p - t0) / (t1 - t0) } else { 0.0 };
    let eased = x * x * (3.0 - 2.0 * x); // smoothstep ≈ ease-in-out
    v0 + (v1 - v0) * eased
}

// NSBox's custom type gives a filled, rounded rect without touching layers.
fn rounded_box(mtm: MainThreadMarker, frame: NSRect, radius: f64, fill: &NSColor) -> Retained<NSBox> {
    let b = NSBox::initWithFrame(NSBox::alloc(mtm), frame);
    b.setBoxType(NSBoxType::Custom);
    b.setTitlePosition(NSTitlePosition::NoTitle);
    b.setContentViewMargins(NSSize::new(0.0, 0.0));
    b.setBorderWidth(0.0);
    b.setCornerRadius(radius);
    b.setFillColor(fill);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_rises_with_voice_above_the_noise_floor() {
        // Room noise sets the floor and reads as silence.
        let (mut level, mut floor) = (0.0, 0.0);
        for _ in 0..30 {
            (level, floor) = smooth_level(0.002, level, floor);
        }
        assert!(level < 0.01 && (floor - 20.0 * 0.002f64.log10()).abs() < 0.01);
        // Speech 30 dB above it saturates and turns the bars solid.
        for _ in 0..30 {
            (level, floor) = smooth_level(0.07, level, floor);
        }
        assert!(level > VOICE && level <= 1.0, "{level}");
        // Digital silence can't drag the floor below -60 dB.
        assert_eq!(smooth_level(0.0, 0.0, 0.0).1, NOISE_FLOOR_MIN_DB);
    }

    #[test]
    fn bars_stay_in_bounds_and_peak_in_the_middle() {
        for step in 0..100 {
            let t = step as f64 * 0.013;
            for level in [0.0, 0.3, 1.0] {
                assert!(level_heights(level, t).iter().all(|h| (BAR_MIN..=BAR_MAX).contains(h)));
            }
            assert!(
                thinking_heights(t)
                    .iter()
                    .all(|h| (BAR_MIN..=BAR_MIN + 4.0).contains(h))
            );
        }
        // Silent: just a gentle idle breathing near the minimum.
        assert!(level_heights(0.0, 0.4).iter().all(|h| *h <= BAR_MIN * 1.5));
        // At full level the middle bar outgrows the edges whatever the breathing phase.
        let avg = |i: usize| (0..100).map(|s| level_heights(1.0, s as f64 * 0.01)[i]).sum::<f64>();
        assert!(avg(BARS / 2) > 1.5 * avg(0) && avg(BARS / 2) > 1.5 * avg(BARS - 1));
    }

    #[test]
    fn breath_hits_keyframes_and_wraps() {
        assert!((breath(0.0) - 1.0).abs() < 1e-9);
        assert!((breath(0.4) - 1.5).abs() < 1e-9);
        assert!((breath(1.4) - 1.5).abs() < 1e-9);
        assert!((breath(-0.6) - 1.5).abs() < 1e-9);
    }
}
