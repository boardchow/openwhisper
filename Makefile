# OpenWhisper — local voice dictation for macOS.
#   make run      build, bundle, sign and launch dist/OpenWhisper.app
#   make install  same, but copy into /Applications

APP        := dist/OpenWhisper.app
MODEL_DIR  := $(HOME)/Library/Application Support/OpenWhisper/models
MODELS     := ggml-large-v3-turbo.bin ggml-small.bin
MODEL_URL  := https://huggingface.co/ggerganov/whisper.cpp/resolve/main
# macOS ties Accessibility/Microphone grants to the code signature. Ad-hoc
# signing changes it every build (grants silently go stale), so we sign with a
# stable self-signed identity created once by build/setup-signing.sh.
SIGN_IDENTITY := OpenWhisper Local Dev
SIGN_KEYCHAIN := openwhisper-signing.keychain-db

.PHONY: app run install model test clean

app:
	@security find-identity "$(SIGN_KEYCHAIN)" 2>/dev/null | grep -q "$(SIGN_IDENTITY)" || bash build/setup-signing.sh
	cargo build --release
	rm -rf $(APP) && mkdir -p $(APP)/Contents/MacOS
	cp target/release/openwhisper $(APP)/Contents/MacOS/
	cp build/Info.plist $(APP)/Contents/
	@security unlock-keychain -p openwhisper "$(SIGN_KEYCHAIN)"
	codesign --force --sign "$(SIGN_IDENTITY)" --keychain "$(SIGN_KEYCHAIN)" --entitlements build/entitlements.plist $(APP)

# large-v3-turbo (1.5 GB) for the final transcript, small (466 MB) for live typing.
model:
	@mkdir -p "$(MODEL_DIR)"
	@for m in $(MODELS); do test -f "$(MODEL_DIR)/$$m" || curl -L --fail -o "$(MODEL_DIR)/$$m" "$(MODEL_URL)/$$m"; done

# Launch through `open` so macOS attributes permissions to the app, not the terminal.
run: app model
	@pkill -x openwhisper || true; while pgrep -x openwhisper >/dev/null; do sleep 0.1; done
	@# LaunchServices can briefly still think the old instance runs (error -600); retry once.
	open $(APP) || (sleep 1 && open $(APP))

install: app model
	@pkill -x openwhisper || true; while pgrep -x openwhisper >/dev/null; do sleep 0.1; done
	rm -rf /Applications/OpenWhisper.app && cp -R $(APP) /Applications/
	open /Applications/OpenWhisper.app

test:
	cargo test

clean:
	rm -rf dist target
