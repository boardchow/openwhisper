//! OpenWhisper: tap right ⌘, speak, tap again. A small pill floats at the
//! bottom of the screen while recording, and whisper.cpp transcribes on-device
//! as you speak, typing into whatever field has focus.

mod audio;
mod hotkey;
mod live;
mod overlay;
mod settings;

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use objc2::{MainThreadMarker, MainThreadOnly, sel};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSApplicationWillTerminateNotification, NSImage, NSMenu,
    NSMenuItem, NSPasteboard, NSPasteboardTypeString, NSStatusBar, NSVariableStatusItemLength, NSWorkspace,
};
use objc2_foundation::{NSNotificationCenter, NSString, NSTimer, ns_string};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

const MODELS: &str = "Library/Application Support/OpenWhisper/models";
// Live passes need speed (~0.25 s each on Apple Silicon); the final pass needs accuracy.
const LIVE_MODEL: &str = "ggml-small.bin";
const FINAL_MODEL: &str = "ggml-large-v3-turbo.bin";
const KEY_DELETE: u16 = 51;
// shortcut: every pass re-transcribes the whole clip, so passes slow down on dictations over a
// minute or so; upgrade by dropping audio already committed, using segment timestamps.
const PASS_EVERY: Duration = Duration::from_millis(150);
// Auto-detect waits for ~1.5 s of speech (2 s including the margins trim_silence keeps):
// on a breath or a pause it guesses anything.
const DETECT_AFTER: usize = audio::WHISPER_RATE as usize * 2;

type Whisper = Arc<Mutex<WhisperState>>;

enum State {
    Idle,
    Recording { rec: audio::Recording, live: Live },
    Transcribing { rx: Receiver<String>, live: Live },
}

/// Text typed into the focused field so far, plus the live pass in flight.
struct Live {
    typed: String,
    last: String,
    pass: Option<Receiver<String>>,
    pass_at: Instant,
    // The user's languages, narrowed to one once detected from the first 1.5 s
    // of speech; auto-detecting on every pass would cost a second encoder run.
    langs: Vec<&'static str>,
    detecting: Option<Receiver<&'static str>>,
    // On the desktop there's no field, and keystrokes would trigger Finder's type-select.
    to_clipboard: bool,
}

/// An edit in the focused field: delete this many characters, then type this.
type Edit = (usize, String);

impl Live {
    fn new(langs: Vec<&'static str>, to_clipboard: bool) -> Self {
        Live {
            typed: String::new(),
            last: String::new(),
            pass: None,
            pass_at: Instant::now(),
            langs,
            detecting: None,
            to_clipboard,
        }
    }

    /// The edit that types (or corrects) the words this pass and the previous one agree on.
    fn commit(&mut self, text: String) -> Option<Edit> {
        let step = live::step(&self.typed, &self.last, &text);
        self.last = text;
        let (back, add, typed) = step.filter(|_| !self.to_clipboard)?;
        self.typed = typed;
        Some((back, add))
    }

    /// The edit that brings the field in line with the final transcript; None
    /// if the final pass came back empty, so it can't erase what was typed.
    fn finish(&self, text: &str) -> Option<Edit> {
        (!text.is_empty()).then(|| live::edit(&self.typed, text))
    }

    /// The edit that erases what was typed so far (Esc).
    fn cancel(&self) -> Edit {
        (self.typed.chars().count(), String::new())
    }
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let dir = format!("{}/{MODELS}", std::env::var("HOME").unwrap_or_default());
    let (live_model, final_model) = match (load_whisper(&dir, LIVE_MODEL), load_whisper(&dir, FINAL_MODEL)) {
        (Ok(l), Ok(f)) => (l, f),
        (Err(e), _) | (_, Err(e)) => return fatal(mtm, &format!("{e}\n\nRun `make model` to download the models.")),
    };
    warm_up(&live_model);
    warm_up(&final_model);
    // whisper.cpp's exit-time teardown aborts while models are loaded or a pass is
    // running, so quit without it; macOS reclaims the GPU memory anyway.
    let quit = RcBlock::new(|_| unsafe { _exit(0) });
    let _quit_observer = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(NSApplicationWillTerminateNotification),
            None,
            None,
            &quit,
        )
    };

    let status = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = status.button(mtm) {
        // An SF Symbol renders at the same size as the system's menu-bar icons and adapts to light/dark.
        match NSImage::imageWithSystemSymbolName_accessibilityDescription(
            ns_string!("waveform"),
            Some(ns_string!("OpenWhisper")),
        ) {
            Some(icon) => button.setImage(Some(&icon)),
            None => button.setTitle(ns_string!("◉")),
        }
    }
    let menu = NSMenu::new(mtm);
    let quit = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("Quit OpenWhisper"),
            Some(sel!(terminate:)),
            ns_string!("q"),
        )
    };
    settings::load_shortcut();
    let (items, _picker) = settings::menus(mtm);
    for item in &items {
        menu.addItem(item);
    }
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&quit);
    status.setMenu(Some(&menu));

    ask_for_accessibility();
    let level = Arc::new(AtomicU32::new(0));
    let pill = overlay::Pill::new(mtm);
    let started = Instant::now();
    let tap = RefCell::new(None);
    let state = RefCell::new(State::Idle);

    let tick = RcBlock::new(move |_: std::ptr::NonNull<NSTimer>| {
        // Retry until Accessibility is granted, so no relaunch is needed.
        let mut tap = tap.borrow_mut();
        match tap.as_ref() {
            None => *tap = hotkey::install(),
            // macOS disables taps it deems slow; re-enabling is a no-op otherwise.
            Some(t) => t.enable(),
        }

        let mut state = state.borrow_mut();
        if hotkey::CANCELLED.swap(false, Ordering::SeqCst) {
            // Dropping the recording stops the mic; in-flight passes finish into closed channels.
            if let State::Recording { live, .. } | State::Transcribing { live, .. } =
                std::mem::replace(&mut *state, State::Idle)
            {
                pill.hide();
                let (back, add) = live.cancel();
                type_text(back, &add);
            }
        }
        if hotkey::TAPPED.swap(false, Ordering::SeqCst) {
            *state = match std::mem::replace(&mut *state, State::Idle) {
                State::Idle => match audio::start(level.clone()) {
                    Ok(rec) => {
                        pill.show();
                        State::Recording {
                            rec,
                            live: Live::new(settings::languages(), frontmost_is_finder()),
                        }
                    }
                    Err(e) => {
                        eprintln!("record: {e}");
                        State::Idle
                    }
                },
                State::Recording { rec, live } => {
                    let (samples, langs) = (rec.finish(), live.langs.clone());
                    State::Transcribing {
                        rx: spawn(&final_model, move |s| transcribe(s, samples, &langs)),
                        live,
                    }
                }
                busy => busy, // ignore taps while transcribing
            };
        }

        let t = started.elapsed().as_secs_f64();
        match &mut *state {
            State::Idle => {}
            State::Recording { rec, live } => {
                pill.draw_level(f32::from_bits(level.load(Ordering::Relaxed)), t);
                match &live.detecting {
                    _ if live.langs.len() == 1 => {}
                    None => {
                        let samples = rec.snapshot();
                        if audio::trim_silence(&samples).len() >= DETECT_AFTER {
                            let langs = live.langs.clone();
                            live.detecting = Some(spawn(&live_model, move |s| detect_language(s, &samples, &langs)));
                        }
                    }
                    Some(rx) => match rx.try_recv() {
                        Ok(lang) => live.langs = vec![lang],
                        Err(TryRecvError::Disconnected) => live.langs.truncate(1),
                        Err(TryRecvError::Empty) => {}
                    },
                }
                if let Some(rx) = &live.pass {
                    match rx.try_recv() {
                        Ok(pass) => {
                            if let Some((back, add)) = live.commit(pass) {
                                type_text(back, &add);
                            }
                            live.pass = None;
                        }
                        Err(TryRecvError::Disconnected) => live.pass = None,
                        Err(TryRecvError::Empty) => {}
                    }
                }
                if live.langs.len() == 1 && live.pass.is_none() && live.pass_at.elapsed() >= PASS_EVERY {
                    let (samples, langs) = (rec.snapshot(), live.langs.clone());
                    live.pass = Some(spawn(&live_model, move |s| transcribe(s, samples, &langs)));
                    live.pass_at = Instant::now();
                }
            }
            State::Transcribing { rx, live } => match rx.try_recv() {
                Err(TryRecvError::Empty) => pill.draw_thinking(t),
                done => {
                    pill.hide();
                    match done {
                        Ok(text) if live.to_clipboard => copy(&text),
                        Ok(text) => {
                            if let Some((back, add)) = live.finish(&text) {
                                type_text(back, &add);
                            }
                        }
                        Err(_) => {}
                    }
                    *state = State::Idle;
                }
            },
        }
        hotkey::ACTIVE.store(!matches!(*state, State::Idle), Ordering::SeqCst);
    });
    // SAFETY: a scheduled timer fires on the run loop it was added to, the main one here.
    let _timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0 / 60.0, true, &tick) };
    app.run();
}

/// Runs `job` with the model on a worker thread; jobs on one model queue on its lock.
fn spawn<T: Send + 'static>(
    whisper: &Whisper,
    job: impl FnOnce(&mut WhisperState) -> T + Send + 'static,
) -> Receiver<T> {
    let (tx, rx) = mpsc::channel();
    let whisper = whisper.clone();
    std::thread::spawn(move || {
        let _ = tx.send(job(&mut whisper.lock().unwrap()));
    });
    rx
}

/// Loads a model onto the GPU (Metal, flash attention).
fn load_whisper(dir: &str, name: &str) -> Result<Whisper, String> {
    let path = format!("{dir}/{name}");
    let mut params = WhisperContextParameters::default();
    params.use_gpu(true).flash_attn(true);
    let load = |e: whisper_rs::WhisperError| format!("Could not load the speech model at {path}: {e}");
    let ctx = WhisperContext::new_with_params(&path, params).map_err(load)?;
    Ok(Arc::new(Mutex::new(ctx.create_state().map_err(load)?)))
}

/// Runs one pass in the background, so the first dictation doesn't pay
/// Metal's one-time warm-up (~1 s).
fn warm_up(whisper: &Whisper) {
    // A tone, not silence: trim_silence would cut silence to nothing and skip the run.
    let tone = (0..audio::WHISPER_RATE)
        .map(|i| 0.1 * (i as f32 * 0.17).sin())
        .collect();
    drop(spawn(whisper, |s| transcribe(s, tone, &["en"])));
}

fn threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().min(8))
}

/// The most likely of `langs` for this audio (the first one if detection fails).
fn detect_language(state: &mut WhisperState, samples: &[f32], langs: &[&'static str]) -> &'static str {
    let detected = state
        .pcm_to_mel(audio::trim_silence(samples), threads())
        .and_then(|_| state.lang_detect(0, threads()));
    detected.map_or(langs[0], |(_, probs)| settings::most_likely(&probs, langs))
}

/// Transcribes in `langs[0]` if it's the only one, else in whichever of `langs` is detected.
fn transcribe(state: &mut WhisperState, samples: Vec<f32>, langs: &[&'static str]) -> String {
    let rate = audio::WHISPER_RATE as usize;
    let mut samples = audio::trim_silence(&samples).to_vec();
    if samples.len() < rate / 4 {
        return String::new(); // silence, or an accidental double tap
    }
    // whisper.cpp refuses clips under 1 s; pad with silence.
    samples.resize(samples.len().max(rate + rate / 10), 0.0);

    let lang = if langs.len() == 1 {
        langs[0]
    } else {
        detect_language(state, &samples, langs)
    };
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some(lang));
    params.set_no_context(true);
    params.set_suppress_blank(true);
    params.set_n_threads(threads() as i32);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_special(false);
    params.set_print_timestamps(false);
    if let Err(e) = state.full(params, &samples) {
        eprintln!("transcribe: {e}");
        return String::new();
    }
    let text: String = state.as_iter().map(|s| s.to_string()).collect();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    // Whisper's marker for "heard nothing".
    if text == "[BLANK_AUDIO]" { String::new() } else { text }
}

/// Types into the focused app with synthetic key events, after `backspaces`
/// deletes. The pill never takes focus, so the field the user was in still has it.
fn type_text(backspaces: usize, text: &str) {
    let Ok(source) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        return;
    };
    let key = |code: u16, chars: &str| {
        for down in [true, false] {
            if let Ok(event) = CGEvent::new_keyboard_event(source.clone(), code, down) {
                event.set_flags(CGEventFlags::CGEventFlagNull);
                if !chars.is_empty() {
                    event.set_string(chars);
                }
                event.post(CGEventTapLocation::HID);
            }
        }
    };
    for _ in 0..backspaces {
        key(KEY_DELETE, "");
    }
    // One event carries at most 20 UTF-16 units; 10 chars always fit.
    let chars: Vec<char> = text.chars().collect();
    for chunk in chars.chunks(10) {
        key(0, &chunk.iter().collect::<String>());
    }
}

fn copy(text: &str) {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    pasteboard.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
}

fn frontmost_is_finder() -> bool {
    let front = NSWorkspace::sharedWorkspace().frontmostApplication();
    front
        .and_then(|a| a.bundleIdentifier())
        .is_some_and(|id| id.to_string() == "com.apple.finder")
}

unsafe extern "C" {
    fn _exit(status: i32) -> !;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    static kAXTrustedCheckOptionPrompt: CFStringRef;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
}

/// Shows the system "grant Accessibility" prompt if the app isn't trusted yet.
fn ask_for_accessibility() {
    let key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
    let opts = CFDictionary::from_CFType_pairs(&[(key, CFBoolean::true_value())]);
    unsafe { AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef()) };
}

fn fatal(mtm: MainThreadMarker, message: &str) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(ns_string!("OpenWhisper"));
    alert.setInformativeText(&NSString::from_str(message));
    NSApplication::sharedApplication(mtm).activate();
    alert.runModal();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_types_agreed_words_then_the_final_correction() {
        let mut live = Live::new(vec!["en"], false);
        assert_eq!(live.commit("Hello world...".into()), None); // nothing agreed yet
        assert_eq!(live.commit("Hello world, how".into()), Some((0, "Hello".into())));
        assert_eq!(
            live.commit("Hello world, how are".into()),
            Some((0, " world, how".into()))
        );
        assert_eq!(live.finish("Hello world, how are you?"), Some((0, " are you?".into())));
        assert_eq!(live.finish(""), None); // a failed final pass keeps the text
        assert_eq!(live.cancel(), (16, String::new())); // "Hello world, how"
    }

    #[test]
    fn live_types_nothing_on_the_desktop() {
        let mut live = Live::new(vec!["en"], true);
        live.commit("Hello world".into());
        assert_eq!(live.commit("Hello world".into()), None);
        assert_eq!(live.cancel(), (0, String::new()));
    }

    #[test]
    fn a_missing_model_is_reported_with_its_path() {
        let err = load_whisper("/nonexistent", "ggml-none.bin").err().unwrap();
        assert!(err.contains("/nonexistent/ggml-none.bin"), "{err}");
    }

    /// Speech from macOS's `say`, as 16 kHz mono samples.
    fn say(voice: &str, text: &str) -> Vec<f32> {
        let path = std::env::temp_dir().join(format!("openwhisper-test-{voice}.wav"));
        let ok = std::process::Command::new("say")
            .args(["-v", voice, "--data-format=LEF32@16000", "-o"])
            .arg(&path)
            .arg(text)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "say failed");
        let bytes = std::fs::read(&path).unwrap();
        let data = bytes.windows(4).position(|w| w == b"data").unwrap() + 8;
        bytes[data..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect()
    }

    #[test]
    fn transcribes_speech_in_the_detected_language() {
        let dir = format!("{}/{MODELS}", std::env::var("HOME").unwrap_or_default());
        let Ok(model) = load_whisper(&dir, LIVE_MODEL) else {
            eprintln!("skipped: {LIVE_MODEL} not downloaded (run `make model`)");
            return;
        };
        let state = &mut model.lock().unwrap();
        let english = say("Samantha", "This is a dictation test.");
        let french = say("Thomas", "Bonjour, ceci est un test de dictée.");

        assert_eq!(detect_language(state, &english, &["en", "fr"]), "en");
        assert_eq!(detect_language(state, &french, &["en", "fr"]), "fr");
        assert!(
            transcribe(state, english, &["en", "fr"])
                .to_lowercase()
                .contains("dictation")
        );
        assert!(transcribe(state, french, &["fr"]).to_lowercase().contains("bonjour"));
        // Silence and accidental double taps type nothing.
        assert_eq!(transcribe(state, vec![0.0; 32_000], &["en"]), "");
        assert_eq!(transcribe(state, vec![0.1; 1_000], &["en"]), "");
    }
}
