//! Menu-bar settings, remembered in user defaults.
//!
//! Languages: the ones the user speaks, defaulting to the Mac's main language.
//! Same model as Wispr Flow and Superwhisper: one language is forced; with
//! several, detection only chooses among them. Unrestricted, whisper picks from
//! ~99 languages and on a breath or an accent can land anywhere (Arabic for
//! English, say).
//!
//! Shortcut: which modifier key's tap toggles dictation, e.g. to avoid Claude
//! Desktop, which also uses right ⌘.

use std::sync::atomic::Ordering;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSControlStateValueOff, NSControlStateValueOn, NSMenu, NSMenuItem};
use objc2_foundation::{NSLocale, NSString, NSUserDefaults, ns_string};

use crate::hotkey;

const LANGUAGES: [(&str, &str); 10] = [
    ("en", "English"),
    ("fr", "Français"),
    ("es", "Español"),
    ("de", "Deutsch"),
    ("it", "Italiano"),
    ("pt", "Português"),
    ("nl", "Nederlands"),
    ("ja", "日本語"),
    ("zh", "中文"),
    ("ar", "العربية"),
];

/// The ticked language codes, in menu order; never empty.
pub fn languages() -> Vec<&'static str> {
    let saved = NSUserDefaults::standardUserDefaults()
        .stringForKey(ns_string!("languages"))
        .map(|s| s.to_string());
    let main = NSLocale::preferredLanguages().firstObject().map(|l| l.to_string());
    parse_languages(saved.as_deref(), main.as_deref())
}

/// The menu's codes listed in `saved` (comma-separated) or, if nothing is
/// saved, the Mac's `main` language ("en-US" → "en"); English if none match.
fn parse_languages(saved: Option<&str>, main: Option<&str>) -> Vec<&'static str> {
    // Like Wispr Flow: by default just the main language, so nothing is auto-detected until the user ticks more.
    let wanted: Vec<&str> = match saved {
        Some(list) => list.split(',').collect(),
        None => main.map(|m| m.get(..2).unwrap_or(m)).into_iter().collect(),
    };
    let picked: Vec<_> = LANGUAGES
        .iter()
        .map(|(c, _)| *c)
        .filter(|c| wanted.contains(c))
        .collect();
    if picked.is_empty() { vec!["en"] } else { picked }
}

/// The most likely of `langs`, given whisper's per-language probabilities.
pub fn most_likely(probs: &[f32], langs: &[&'static str]) -> &'static str {
    let prob = |code: &str| {
        whisper_rs::get_lang_id(code)
            .and_then(|id| probs.get(id as usize))
            .copied()
            .unwrap_or(0.0)
    };
    langs
        .iter()
        .copied()
        .min_by(|a, b| prob(b).total_cmp(&prob(a))) // highest; ties go to the first
        .unwrap_or("en")
}

/// Applies the saved shortcut key to the hotkey tap.
pub fn load_shortcut() {
    let saved = NSUserDefaults::standardUserDefaults()
        .stringForKey(ns_string!("shortcut"))
        .map(|s| s.to_string());
    if let Some(key) = parse_shortcut(saved.as_deref()) {
        hotkey::KEY.store(key, Ordering::Relaxed);
    }
}

/// `langs` with `code` ticked or unticked; the last one can't be unticked.
fn toggle(mut langs: Vec<&'static str>, code: &'static str) -> Vec<&'static str> {
    match langs.iter().position(|c| *c == code) {
        Some(_) if langs.len() == 1 => {}
        Some(i) => drop(langs.remove(i)),
        None => langs.push(code),
    }
    langs
}

/// A saved keycode, if it's one of the menu's keys.
fn parse_shortcut(saved: Option<&str>) -> Option<i64> {
    saved?.parse().ok().filter(|k| hotkey::KEYS.iter().any(|(c, _)| c == k))
}

fn save(key: &NSString, value: &str) {
    unsafe { NSUserDefaults::standardUserDefaults().setObject_forKey(Some(&NSString::from_str(value)), key) };
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and Picker has no Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    pub struct Picker;

    impl Picker {
        // SAFETY: both match NSMenuItem's action signature.
        #[unsafe(method(language:))]
        fn language(&self, item: &NSMenuItem) {
            let langs = toggle(languages(), LANGUAGES[item.tag() as usize].0);
            save(ns_string!("languages"), &langs.join(","));
            refresh(item, |i| languages().contains(&LANGUAGES[i].0));
        }

        #[unsafe(method(shortcut:))]
        fn shortcut(&self, item: &NSMenuItem) {
            let key = hotkey::KEYS[item.tag() as usize].0;
            hotkey::KEY.store(key, Ordering::Relaxed);
            save(ns_string!("shortcut"), &key.to_string());
            refresh(item, |i| hotkey::KEYS[i].0 == key);
        }
    }
);

/// The "Languages" and "Shortcut" items. Menu items hold their target weakly,
/// so the caller must keep the returned Picker alive.
pub fn menus(mtm: MainThreadMarker) -> ([Retained<NSMenuItem>; 2], Retained<Picker>) {
    let picker: Retained<Picker> = unsafe { msg_send![Picker::alloc(mtm), init] };
    let names: Vec<_> = LANGUAGES.iter().map(|(_, name)| *name).collect();
    let langs = submenu(mtm, &picker, "Languages", &names, sel!(language:), |i| {
        languages().contains(&LANGUAGES[i].0)
    });
    let names: Vec<_> = hotkey::KEYS.iter().map(|(_, name)| *name).collect();
    let key = hotkey::KEY.load(Ordering::Relaxed);
    let shortcut = submenu(mtm, &picker, "Shortcut", &names, sel!(shortcut:), |i| {
        hotkey::KEYS[i].0 == key
    });
    ([langs, shortcut], picker)
}

fn menu_item(mtm: MainThreadMarker, target: &Picker, title: &str, action: Sel) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            ns_string!(""),
        )
    };
    unsafe { item.setTarget(Some(target)) };
    item
}

fn submenu(
    mtm: MainThreadMarker,
    picker: &Picker,
    title: &str,
    names: &[&str],
    action: Sel,
    on: impl Fn(usize) -> bool,
) -> Retained<NSMenuItem> {
    let choices = NSMenu::new(mtm);
    for (i, name) in names.iter().enumerate() {
        let item = menu_item(mtm, picker, name, action);
        item.setTag(i as isize);
        choices.addItem(&item);
    }
    let parent = NSMenuItem::new(mtm);
    parent.setTitle(&NSString::from_str(title));
    parent.setSubmenu(Some(&choices));
    check(&choices, on);
    parent
}

/// Re-ticks the items of `item`'s menu.
fn refresh(item: &NSMenuItem, on: impl Fn(usize) -> bool) {
    if let Some(menu) = unsafe { item.menu() } {
        check(&menu, on);
    }
}

fn check(menu: &NSMenu, on: impl Fn(usize) -> bool) {
    for item in menu.itemArray() {
        item.setState(if on(item.tag() as usize) {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_come_from_the_saved_list_or_the_macs_main_language() {
        assert_eq!(parse_languages(Some("fr,en,xx"), Some("de-DE")), ["en", "fr"]); // menu order, unknown dropped
        assert_eq!(parse_languages(None, Some("fr-ES")), ["fr"]);
        assert_eq!(parse_languages(None, Some("x")), ["en"]);
        assert_eq!(parse_languages(None, None), ["en"]);
        assert_eq!(parse_languages(Some(""), Some("fr")), ["en"]);
    }

    #[test]
    fn toggling_keeps_at_least_one_language() {
        assert_eq!(toggle(vec!["en"], "fr"), ["en", "fr"]);
        assert_eq!(toggle(vec!["en", "fr"], "en"), ["fr"]);
        assert_eq!(toggle(vec!["en"], "en"), ["en"]);
    }

    #[test]
    fn shortcut_must_be_one_of_the_menus_keys() {
        assert_eq!(parse_shortcut(Some("61")), Some(61));
        assert_eq!(parse_shortcut(Some("12")), None);
        assert_eq!(parse_shortcut(Some("cmd")), None);
        assert_eq!(parse_shortcut(None), None);
    }

    #[test]
    fn most_likely_only_considers_the_users_languages() {
        let mut probs = vec![0.0; 100];
        let id = |code| whisper_rs::get_lang_id(code).unwrap() as usize;
        probs[id("ar")] = 0.7;
        probs[id("fr")] = 0.2;
        probs[id("en")] = 0.1;
        assert_eq!(most_likely(&probs, &["en", "fr"]), "fr");
        assert_eq!(most_likely(&probs, &["en", "es"]), "en");
        assert_eq!(most_likely(&[], &["es", "en"]), "es");
    }
}
