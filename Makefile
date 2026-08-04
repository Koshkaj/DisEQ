APP := target/DisEQ.app
BIN := DisEQ

.DEFAULT_GOAL := help
.PHONY: help build release check test fmt lint app app-release run stop probe reconnect ddc backends selftest audio-probe eq-probe driver driver-install driver-uninstall driver-probe route mixer clean mirror-probe

help: ## Show this help
	@grep -hE '^[a-z-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk -F':.*?## ' '{printf "  \033[1m%-14s\033[0m %s\n", $$1, $$2}'
	@echo ""
	@echo "  Requires: Rust 1.85+, macOS 14+, Xcode command line tools."
	@echo "  The app must run from a bundle — a bare 'cargo run' gets no status item."

# --- build ------------------------------------------------------------------

build: ## Compile the workspace (debug)
	cargo build --workspace

release: ## Compile the workspace (release, LTO + stripped)
	cargo build --workspace --release

check: ## Type-check everything including examples
	cargo check --workspace --examples

test: ## Run the test suite, including the offline EQ response checks
	cargo test --workspace

fmt: ## Format all sources
	cargo fmt --all

lint: ## Run clippy over the workspace
	cargo clippy --workspace --examples -- -D warnings

# --- bundle -----------------------------------------------------------------

app: ## Build DisEQ.app (debug) and ad-hoc sign it
	./bundle/build_app.sh

app-release: ## Build DisEQ.app (release) and ad-hoc sign it
	./bundle/build_app.sh release

run: app ## Rebuild the bundle, restart the app, leave it in the menu bar
	@pkill -f $(BIN) 2>/dev/null || true
	@sleep 1
	@open $(APP)
	@echo "running — click the display icon in the menu bar"

stop: ## Quit the running app
	@pkill -f $(BIN) 2>/dev/null || true

# --- driver -----------------------------------------------------------------

driver: ## Build DisEQ.driver (universal, ad-hoc signed). No privileges needed.
	./driver/build_driver.sh

# Copies into /Library/Audio/Plug-Ins/HAL and restarts coreaudiod, so it asks
# for your password and drops all audio on the machine for about a second.
driver-install: driver ## Install the HAL plug-in (needs admin; restarts coreaudiod)
	./driver/install_driver.sh

driver-uninstall: ## Remove the HAL plug-in (needs admin; restarts coreaudiod)
	./driver/uninstall_driver.sh

# --- probes -----------------------------------------------------------------

probe: ## Dump every attached display: modes, native mode, scale
	cargo run -p kd-core --example probe

reconnect: ## Re-enable every display that was switched off and left off
	cargo run -p kd-core --example reconnect

ddc: ## Read-only DDC/CI probe over the worker thread. Writes nothing.
	cargo run -p kd-core --example ddc

backends: ## Report which backend drives each control on this machine
	cargo run -p kd-core --example backends

selftest: ## Exercise every backend end to end, restoring what it changes
	cargo run -p kd-core --example selftest

audio-probe: ## List output devices and whether their volume is actually settable
	cargo run -p kd-core --example audio_probe

# Renders offline, so it neither needs nor disturbs a real output device.
# Any preset id works, and --auto-preamp shows the gain-staged version:
#   cargo run -p kd-audio --example eq_probe -- rock --auto-preamp
eq-probe: ## Measure what a preset does to a signal, band by band
	cargo run -p kd-audio --example eq_probe -- $(or $(PRESET),bass-booster)

# Deliberately does not set KD_MIRROR_PROBE. This probe briefly mirrors a
# display, so arming it stays an explicit act:
#   KD_MIRROR_PROBE=1 make mirror-probe
# It also refuses to run with fewer than two displays, and never targets the
# main display.
driver-probe: ## Report whether the HAL plug-in is installed, loaded and settable
	cargo run -q -p kd-audio --example driver_probe

# Takes over the default output for a minute and hands it back. Needs the
# driver installed. Pass a device name or flags through ARGS:
#   make route ARGS='"DELL U2720Q" --preset bass-booster --volume 0.5'
#   make route ARGS='--list'
route: ## Route system audio through the EQ to real hardware, reporting drift
	cargo run -q -p kd-audio --example route -- $(ARGS)

# Needs no driver. Without ARGS it only lists what is playing; --run taps those
# applications and fades each one in turn:
#   make mixer ARGS=--run
#   make mixer ARGS='--run --gain spotify=0.2'
mixer: ## Per-application volume through process taps
	cargo run -q -p kd-audio --example mixer -- $(ARGS)

mirror-probe: ## Test the mirror-based soft disable (needs 2+ displays; see target comment)
	cargo run -p kd-core --example mirror_probe

# --- housekeeping -----------------------------------------------------------

clean: ## Remove build artefacts and the bundle
	cargo clean
	rm -rf $(APP)
