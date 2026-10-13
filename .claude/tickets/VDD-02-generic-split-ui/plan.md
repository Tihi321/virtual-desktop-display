# Plan: VDD-02 — split any display, with settings UI

Ticket folder: `C:\projects\Personal\virtual-desktop-display\.claude\tickets\VDD-02-generic-split-ui\` (plan copied there at implementation start).
Branch: `VDD-02_generic_split_ui`, created from `master` at `1e69804` (contains all v0.1 work; already on origin/master).

## Context

v0.1 (ticket `flipgo-virtual-split`) works end to end, but it is hard-wired:

- one display, matched by the name "FlipGo";
- exactly 2 stacked pieces at 1920×1080;
- the hidden display parked above the primary;
- config only through TOML.

The user wants a general tool that anyone can use:

- split **any detected display** into columns, rows, a grid or custom ratios;
- run several splits at once, including the main display, with a choosable "main piece";
- a **settings UI** with a detected-display dropdown and a layout preview;
- driver install help with download links;
- **global hotkeys** that can be changed and enabled/disabled;
- **auto-split when a configured display connects**;
- **GitHub release packaging**.

Constraints:

- **Parsec VDD 0.45 resolutions.** It has a built-in mode list (it includes 1920×1080, 2560×1440, 3440×1440, 1280×720 … but **not** 1720×1440, 1280×1440, 1146×1440). Only **5 custom modes** are allowed in total, stored at `HKLM\SOFTWARE\Parsec\vdd\{0..4}` with DWORDs `width`, `height`, `hz`. Writing them needs admin rights. The driver reads them when a display is added; it does not need a driver restart.
- **Display count.** Max 8 virtual displays (`VDD_MAX_DISPLAYS`). Each ADD takes about 3 s.
- **Redistribution.** The Parsec driver can't be bundled (no redistribution grant), so we link to it. The official installer is https://builds.parsec.app/vdd/parsec-vdd-0.45.0.0.exe (signed by "Parsec Cloud, Inc.").
- **Process-global state.** The portal and rescue use process-global hook state, `SavedLayout` covers the whole topology, and VDD targets are found by diffing. So **one Session must own all splits**: one VDD handle, one saved layout, one portal, one rescue, and one renderer per split display.

## Design

### Config v2 (`src/config.rs`)
```toml
version = 2
autostart = false
start_on_launch = true
[hotkeys]                       # each: keys + enabled; master switch
enabled = true
toggle_all  = { keys = "Ctrl+Alt+S", enabled = true }
cycle_order = { keys = "Ctrl+Alt+W", enabled = true }   # swap/rotate pieces of all running splits
open_settings = { keys = "Ctrl+Alt+D", enabled = false }
[[splits]]
name = "FlipGo"
enabled = true
auto_on_connect = true
display_id = "RTK4707"          # EDID mfr+product from the device path (stable across reboots)
name_match = "FlipGo"           # optional fallback: friendly-name substring (FlipGo is -H/-A per input)
layout = "rows"                 # columns | rows | grid | custom
columns = 1
rows = 2
ratios_x = []                   # custom only, fractions summing to 1
ratios_y = []
order = [0, 1]                  # piece permutation (swap = rotate)
main_piece = 0                  # used only when the display is the primary
refresh_hz = 60
```

- **v1 migration:** `physical_match` becomes one split with `name_match` and rows=2; `swap = true` becomes `order = [1, 0]`.
- **Display identity:** match on `display_id` (parsed from `TargetInfo.device_path`, for example `DISPLAY#RTK4707#…`) first, then on `name_match`. Exclude ParsecVDA displays from detection.

### New pure modules (unit-tested)

**`src/geometry.rs`**
- `pieces(display: Rect, spec) -> Vec<Rect>`: piece rects relative to the display.
- Inner boundaries are rounded to even pixels, and the last piece takes the remainder.
- `order` is applied, and the virtual size equals the piece size.

**`src/layout.rs`** (replaces `displays::compute_layout` / `Layout` / `looks_like_our_layout`)
- `plan(real: &[DisplayGeom], splits: &[SplitGeom]) -> LayoutPlan` returns the virtual positions (piece rect + the display's old origin) and the parking spot for each hidden display.
- **Parking** follows the sprite-safe rule learned in v0.1. The hidden display touches another display only with its **bottom edge on that display's top edge**, or its **right edge on that display's left edge**. Candidates are tried in order (topmost displays first, right-aligned, then left-aligned) and the first with no overlap and no contact with another hidden display wins.
- **Primary split:** the whole plan is translated so `main_piece` sits at (0,0), which makes it the new primary.
- The settle check compares against the plan's normalized positions.

**`src/modes.rs`**
- `BUILTIN` is the parsec 0.45 mode list.
- `required(splits) -> Vec<Mode>` and `plan_slots(required, current_registry) -> SlotPlan`, which reports built-in, already registered, slots to write, or "too many".
- `read_custom_modes()` reads the registry (no admin needed). `write_custom_modes()` is run only from the elevated `--register-modes` helper.
- If more than 5 unique sizes are needed, fall back to the nearest larger built-in mode with a stretch, and warn.

### Runtime refactor (existing files)

**`src/session.rs`**
- `Session::start(cfg, splits: &[ResolvedSplit])` takes all enabled and present splits.
- It ADDs the sum of their pieces in one go (at most 8), partitions the new targets by target id, `set_mode`s each one to its piece size, applies the `layout::plan`, settles, then starts captures, one renderer per split, the portal and the rescue.
- Stop order stays as it is: pipeline, then remove VDDs, then `wait_targets_gone`, then restore.
- `cycle_order(split)` rebuilds the pipeline the same way `set_swap` does today.
- **Speed-up:** skip global `SDC_TOPOLOGY_EXTEND` and inject paths manually after the 1.5 s auto-activate wait. That saves about 4 s and avoids re-enabling displays the user had turned off.

**`src/session_worker.rs`** (new)
- All session work runs on a dedicated worker thread (MTA), driven by commands (Start(set), Stop, Cycle, Reload). Status is posted back to the tray window.
- This keeps the tray and hotkeys responsive during the 10 s+ starts.

**`src/renderer.rs`**
- `Renderer::start(d3d, physical, pieces: Vec<(Arc<MonitorCapture>, Rect dest)>)`.
- Clear to black every frame, then copy (exact size) or stretch per piece.
- Cache SRV/RTV in `Stretch`. Reuse one D3D device per session.

**`src/cursor_portal.rs`**
- `resolve_move(splits: &[PortalSplit{physical, pieces: [(src_rel, virt)]}], prev, new)`.
- The wall applies per hidden display. Points already inside are mapped through the piece under the point.
- Global state holds all splits. Existing tests are re-expressed as N-piece cases.

**`src/window_rescue.rs`**
- A window on any hidden display moves to the piece under its centre, falling back to piece 0. The rest of the logic stays the same.

### App shell

**`src/tray.rs`**
- Menu: **Settings…**, one checkable item per configured split (toggles `enabled` and restarts), Start/Stop all, Cycle pieces, Start with Windows, Exit.
- Double-click opens Settings.
- It handles `RegisterWindowMessageW("VirtualDesktopDisplay.Reload")` from the settings process.
- After each state change it writes `%APPDATA%\virtual-desktop-display\status.json` with: running splits, errors, hotkey registration results, driver state.

**`src/hotkeys.rs`**
- Parses and formats "Ctrl+Alt+S" and uses `RegisterHotKey(tray_hwnd, id, mods | MOD_NOREPEAT, vk)`.
- It re-registers on reload and reports failures, e.g. 1409 "already in use", to `status.json`.
- The master switch and the per-action `enabled` flags are respected.

**`src/autosplit.rs`**
- Debounces `WM_DISPLAYCHANGE` / `WM_DEVICECHANGE`.
- It computes the desired set: enabled splits whose display is present and, if not running, `auto_on_connect`. It restarts the session when the set changes.
- A **manual stop pauses auto-start** for that display until it is unplugged and reconnected.
- It ignores changes while the worker is busy, because our own layout changes fire the same messages.

**`src/driver.rs`**
- `status()`: is the PnP "Parsec Virtual Display Adapter" present, plus the IOCTL version.
- `download_and_install()`: download the official installer to `%TEMP%`, verify Authenticode with `WinVerifyTrust` and signer "Parsec Cloud, Inc.", then launch it with `ShellExecuteW` (UAC).
- Links: the official installer URL, https://github.com/nomi-san/parsec-vdd, and the Parsec support article.

**`src/main.rs`**
- Modes: tray (default), `--settings`, `--register-modes "1720x1440@60,…"` (elevated helper; writes HKLM and exits), `--cli`, `--seconds N`.
- Single-instance mutexes are per mode.
- A second tray launch opens Settings instead of showing the message box.
- First run with no config opens Settings.

### Settings UI (`src/ui/*.rs`, eframe/egui, glow renderer, `default-features = false`)

It runs in a separate `--settings` process, so its winit loop never touches the tray loop.

**Layout preview canvas**
- All detected real displays are drawn to scale at their positions.
- The selected split's display shows its piece lines, piece numbers and a ★ on the main piece.
- Pieces that need a custom resolution get a warning tint.

**Splits panel**
- Add/remove split.
- **Display dropdown** of detected displays: "VG34VQL3A — 3440×1440 — primary", or "(not connected)" for a configured display that is missing.
- Enabled, Auto-split on connect.
- Layout: Columns / Rows / Grid / Custom. Count steppers go up to 4, and the total across all splits is limited to 8.
- Custom-ratio sliders. A rotate/swap order button.
- Main-piece dropdown (only for the primary display). Refresh rate.
- A computed piece table: size, plus "built-in ✓" or "custom slot".

**Driver panel**
- Status, plus a **Download & install** button and links.
- Custom resolution slots (used/needed, out of 5) with a **Register custom resolutions (admin)** button. It warns that this replaces Parsec custom modes.

**Hotkeys panel**
- Master switch.
- Per action: enabled, a "click and press keys" recorder, and live status from `status.json` (registered / in use).

**General panel**
- Start with Windows, Start splitting on launch.

**Footer**
- **Save & apply**: save the config, then post the Reload message, or offer to launch the tray app if it isn't running.
- Start/Stop all.
- The current status line read from `status.json`.

### Packaging

- `README.md` rewritten. It covers: what the tool does and use cases (ultrawide halves for screen-sharing, stacked dual monitors, 4K into a 2×2 grid), requirements, **driver install with links**, the custom-resolution note, the UI walkthrough, hotkeys, config reference, limitations (≈1 frame lag, DRM black, no touch, max 8 virtual displays, 5 custom sizes) and troubleshooting.
- `.github/workflows/ci.yml` runs build, clippy and test on push/PR (windows-latest).
- `.github/workflows/release.yml` triggers on tag `v*`. It runs a release build and publishes `virtual-desktop-display-vX.Y.Z-windows-x64.zip` (exe + README + LICENSE) through a GitHub Release, with actions pinned to exact versions.
- `LICENSE` is MIT. *(Inferred default; change it if you want another license.)* Version bumps to 0.2.0.

## Phases (one implementer per phase; B and C can run in parallel once A has landed)

- **A. Core generalization:** config v2 and migration, `geometry`, `layout`, N-piece and multi-split `session` + worker, renderer, portal, rescue, EXTEND removal. CLI `--cli` keeps working. Tests ported and extended.
  - **Status (A): implemented, not committed.** `cargo build`, `cargo build --examples`, `cargo build --release` (not locked), `cargo clippy --all-targets -- -D warnings` clean; `cargo test`: 52 passed, 0 failed (config 5, geometry 13, layout 11, cursor_portal 11, displays 5, window_rescue 4, vdd 2, autostart 1). E2E not run (orchestrator).
  - Deviations: (1) layout parking prefers REAL non-split displays as attach targets and uses virtual pieces only as a last resort (otherwise the v0.1 FlipGo case would park against the FlipGo's own top piece, not above the Asus); right-aligned "above" candidates are skipped when the hidden display is wider than the target (v0.1 left-aligned fallback). (2) One FrameEvent per split (auto-reset event cannot be shared by several renderers). (3) Worker `Start` takes a config snapshot and resolves splits itself; status is announced by posting `WM_WORKER_STATUS` (WM_APP+10) rather than a registered message; cycling reports `Starting` while it rebuilds. (4) Tray only auto-starts on launch when the config has at least one split. (5) `vdd_probe` needed no change. (6) README config/menu sections updated minimally.
- **B. Modes and driver:** `modes.rs`, the `--register-modes` elevated helper, `driver.rs` (status, download + verify + launch).
  - **Status (B): implemented, not committed.** `cargo build`, `--examples`, `--release`, `clippy --all-targets -D warnings`, `fmt --check` clean; `cargo test`: 71 passed (modes 11, driver 5, layout +2 tile_virtual). `examples/verify_probe` on the genuine installer: OK, signer "Parsec Cloud, Inc."; on an unsigned exe and on Cargo.toml: rejected. WinHTTP download of the installer works (sha256 identical to the saved copy). Not run: UAC helper with a valid list, `--cli` stderr fix (needs adding displays).
  - Deviations/notes: download via WinHTTP (no new crate); session uses `layout::tile_virtual` so a fallback mode larger than its piece never overlaps neighbouring virtual displays (columns/rows grow to the widest/tallest actual display; may overlap real neighbours of the split display in that edge case); `--cli` stderr: attaches parent console and redirects the null stderr handle to CONOUT$.
- **C. Settings UI and IPC:** `ui/`, the `--settings` mode, the Reload message, `status.json`, tray menu changes.
  - **Status (C): implemented, not committed.** eframe/egui 0.36.2 (`glow`, `default_fonts`, no default features) + `serde_json`; `--settings` opened with a temp APPDATA: window lists FlipGo-H 1920x2160 and VG34VQL3A 3440x1440 (primary), preview/piece table/Driver/Hotkeys/Custom sliders verified by screenshot; a second `--settings` exits on its own (raises the first). Not run: Save & apply, tray IPC, UAC register, driver download (all would touch real state/tray).
  - Notes: modules `ui/{app,model,preview,splits,driver,hotkeys,general}.rs`, `ipc.rs` (messages, status.json). Second tray launch runs the settings window in that process. Release exe grew 2.4 MB -> 9.5 MB.
- **D. Hotkeys and auto-split on connect.**
  - **Status (D): implemented, not committed.** `hotkeys.rs` (parse/format, RegisterHotKey on the tray HWND, ids 1..3, MOD_NOREPEAT, 1409 -> "in use by another app"), `autosplit.rs` (pure `decide` + `AutoState`, 13 tests), tray wiring (WM_DISPLAYCHANGE + WM_DEVICECHANGE debounce 1.5 s, re-armed while the worker is Starting/Stopping). `cargo build`, `--examples`, `--release`, `clippy --all-targets -D warnings`, `fmt --check` clean; `cargo test`: 103 passed (autosplit 13, hotkeys 6, ipc 1, ui 12, +others). Hotkey/auto-split behaviour on the real machine not run (orchestrator e2e).
  - Deviations: `decide(state, running, present, cfg)` takes a mutable `AutoState` (armed-per-split flags: start_on_launch, explicit start, connect with auto_on_connect arm; disconnect and manual stop disarm); a failed start is not retried until display presence changes.
- **E. Packaging:** README, workflows, LICENSE, version.

Commit after each phase on `VDD-02_generic_split_ui`. No push or PR unless asked.

## Verification

- `cargo build`, `cargo build --release`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`. New unit tests cover:
  - geometry: columns, rows, grid, custom ratios, even rounding, remainder, order;
  - `layout::plan`: the FlipGo case matches the v0.1 positions; Asus as primary in 2 columns (main piece at 0,0); both at once; no overlaps; sprite-safe parking; connectivity;
  - `modes::plan_slots`;
  - hotkey parse/format;
  - N-piece `resolve_move` and `map_point`;
  - v1 → v2 config migration.
- E2E on this machine, done by the orchestrator with the user watching:
  1. **Regression:** the FlipGo in 2 rows behaves exactly like v0.1 (panels, cursor wall, fullscreen, swap via cycle).
  2. **Asus in 2 columns:** Register custom resolutions creates the 1720×1440 slot (UAC). Both halves are real monitors, the left one is main (taskbar), YouTube fullscreen stays in one half, and Teams/OBS "share screen" lists each half.
  3. Both splits at once (4 virtual displays). Stop restores the original layout. Killing the process recovers it.
  4. **Settings UI:** dropdown shows the detected displays, the preview matches, Save & apply restarts the running splits.
  5. **Hotkeys:** toggle and cycle work, a rebind takes effect, a disabled hotkey doesn't fire, and a conflict shows "in use".
  6. **Auto-split:** unplugging the FlipGo HDMI stops its split cleanly, and replugging starts it again. A manual stop isn't overridden.
  7. **Driver panel:** shows "installed 0.45". Download & install is tested only up to the verified download, unless you want a reinstall.
  8. **CI:** the `ci.yml` syntax is checked locally. The release workflow is only exercised when you push a tag.
