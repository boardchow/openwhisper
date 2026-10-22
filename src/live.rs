//! Live typing while recording. Whisper re-transcribes the whole clip every
//! pass, and its last words keep changing as more audio arrives. Only words two
//! consecutive passes agree on get typed (the "LocalAgreement" policy from
//! whisper_streaming), so text appears steadily instead of flickering.

/// The leading words `prev` and `next` have in common.
pub fn agreed(prev: &str, next: &str) -> String {
    prev.split_whitespace()
        .zip(next.split_whitespace())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a)
        .collect::<Vec<_>>()
        .join(" ")
}

/// What to do after a new pass: `(backspaces, text to type, new typed text)`,
/// or None to wait. Words the passes agree on are typed; if they now agree on a
/// different version of text already typed, the tail is corrected.
pub fn step(typed: &str, prev: &str, next: &str) -> Option<(usize, String, String)> {
    // Whisper ends a clip that stops mid-sentence with "..."; don't type that.
    let [prev, next] = [prev, next].map(|t| t.trim_end_matches(['.', '…']));
    let stable = agreed(prev, next);
    // A shorter agreement is a passing disagreement; wait instead of deleting text.
    if stable.split_whitespace().count() < typed.split_whitespace().count() {
        return None;
    }
    let (back, add) = edit(typed, &stable);
    (back > 0 || !add.is_empty()).then_some((back, add, stable))
}

/// How to turn `typed` into `want`: the number of backspaces, then the text to type.
// shortcut: counts chars, not grapheme clusters; an emoji with modifiers would need more backspaces.
pub fn edit(typed: &str, want: &str) -> (usize, String) {
    let common = typed.chars().zip(want.chars()).take_while(|(a, b)| a == b).count();
    (typed.chars().count() - common, want.chars().skip(common).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agreed_stops_at_first_changed_word() {
        assert_eq!(
            agreed("Hello world, how", "Hello world, how are you"),
            "Hello world, how"
        );
        assert_eq!(agreed("Hello word", "Hello world, how"), "Hello");
        assert_eq!(agreed("", "Hello"), "");
    }

    #[test]
    fn step_types_agreed_words_and_corrects_typed_ones() {
        assert_eq!(
            step("", "Hello world", "Hello world how"),
            Some((0, "Hello world".into(), "Hello world".into()))
        );
        // Already-typed "trying" became "trying to record": keeps going, no "..." typed.
        assert_eq!(step("I'm trying", "I'm trying...", "I'm trying..."), None);
        assert_eq!(
            step("I'm trying", "I'm trying to record", "I'm trying to record my"),
            Some((0, " to record".into(), "I'm trying to record".into()))
        );
        assert_eq!(
            step("Hello wrld", "Hello world is", "Hello world is good"),
            Some((3, "orld is".into(), "Hello world is".into()))
        );
        // One disagreeing pass doesn't delete text.
        assert_eq!(step("Hello there friend", "Hello where", "Hello where is"), None);
    }

    #[test]
    fn edit_appends_or_corrects_the_tail() {
        assert_eq!(edit("Hello", "Hello world"), (0, " world".into()));
        assert_eq!(edit("Hello word", "Hello world."), (1, "ld.".into()));
        assert_eq!(edit("Café", "Cafe"), (1, "e".into()));
        assert_eq!(edit("same", "same"), (0, String::new()));
    }
}
