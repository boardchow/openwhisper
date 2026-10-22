<h1 align="center">OpenWhisper</h1>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-black?style=flat-square" alt="License"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20·%20Apple%20Silicon-black?style=flat-square" alt="Platform">
  <img src="https://img.shields.io/badge/100%25-local-black?style=flat-square" alt="100% local">
  <img src="https://img.shields.io/badge/built%20with-Rust-black?style=flat-square" alt="Rust">
</p>

<p align="center">Open-source, on-device voice dictation for macOS. Tap a key, speak, and watch your words appear wherever you're typing.</p>

Tap **right ⌘** and a small pill appears at the bottom of the screen. Your words are typed into the focused text field as you speak. Tap **right ⌘** again: a last, more accurate pass polishes the text. Everything runs on your Mac.

No cloud. No account. No subscription. No word limits.

## How it works

1. **Tap to record.** A native `CGEventTap` catches a quick tap of the shortcut key; the key keeps working normally in shortcuts like ⌘C.
2. **Speak.** The microphone is captured at 16 kHz, and a lime pill with live bars shows it's listening. It never takes focus from your text field.
3. **Watch it type.** [whisper.cpp](https://github.com/ggml-org/whisper.cpp) (Metal) re-transcribes several times a second with a fast model, and words are typed as soon as two passes agree on them.
4. **Tap again.** A final pass with `large-v3-turbo` corrects whatever needs it. **Esc** cancels and erases what was typed.

## Why OpenWhisper

🔒 **Private by design.** Every transcription happens on your Mac. Your voice never leaves your device.

⚡ **Live.** Text appears while you speak, like Wispr Flow or Superwhisper, without a server round trip.

🌍 **Multilingual.** Pick the languages you speak; with several, it recognises which one you're using.

🪶 **Invisible.** A menu-bar app with a tiny overlay. No windows, no onboarding.

## Install

Requires macOS 13+ on Apple Silicon, [Rust](https://rustup.rs) and CMake (`brew install cmake`).

```sh
git clone https://github.com/boardchow/openwhisper.git
cd openwhisper
make install
```

`make install` builds the app, signs it, downloads the speech models (~2 GB) and installs **OpenWhisper.app** into `/Applications`.

On first launch, macOS asks for two permissions:

- 🎤 **Microphone**, to hear you.
- ♿ **Accessibility**, for the shortcut and for typing into other apps.

## Usage

1. Put your cursor where you want to type.
2. Tap **right ⌘** and speak.
3. Tap **right ⌘** again to finish, or press **Esc** to cancel.

From the waveform icon in the menu bar:

| Setting | |
| --- | --- |
| **Languages** | The languages you speak. One is always used; with several, each recording picks among them. Defaults to your Mac's language. |
| **Shortcut** | Right ⌘, right ⌥, right ⌃ or fn. For fn, set System Settings → Keyboard → "Press 🌐 key to" → Do Nothing. |

The microphone is whichever input macOS has selected.

## Development

```sh
make run    # build, sign and launch from dist/
make test   # unit tests, plus an end-to-end whisper test when the models are downloaded
```

## License

[MIT](LICENSE)
