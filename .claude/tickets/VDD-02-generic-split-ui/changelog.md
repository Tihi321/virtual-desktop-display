# Changelog: VDD-02 generic split + settings UI

## 2026-10-13: VDD-02, v0.2.0

The FlipGo-only splitter is now a general tool: any display can be split into columns, rows, a grid or custom ratios, with a settings UI, hotkeys, auto-split and release packaging. The user tested it end to end and found no regressions.

### Repo `virtual-desktop-display`
Branch `VDD-02_generic_split_ui`, created from `master` at `1e69804`. Commits: `573dab7`, `ae9e76e`, `9c2e40d`, `fd972af`, `d6a0603`, plus the README/changelog commit. In total 33+ files, about 9.7k insertions.

**Core (Phase A)**

| File | What changed |
| --- | --- |
| `src/config.rs` | v2 schema with a `splits` list, hotkeys (master switch plus per-action keys and enabled), autostart and `start_on_launch`. Converts v0.1 files automatically. |
| `src/geometry.rs` (new) | Piece rects for columns, rows, grid and custom ratios. Inner edges round to even pixels, the last piece takes the remainder, and an order permutation is applied. |
| `src/layout.rs` (new) | Layout planner. Virtual pieces go where the physical display was. The hidden display is parked sprite-safe (above or left of a real display). A split primary is translated so the main piece sits at (0,0). `tile_virtual` handles fallback modes. |
| `src/displays.rs` | Adds `display_id` (EDID id from the device path), `is_virtual`, `resolve_split` and `current_refresh`. The global `SDC_TOPOLOGY_EXTEND` step is removed, which made startup about 4–6 s faster. `apply_layout` takes any list of positions. |
| `src/session.rs`, `src/session_worker.rs` (new) | Multi-split session: one virtual display handle, sum of pieces up to 8, one renderer per split, one cursor portal and one window rescue. Runs on a worker thread, so the tray never blocks. |
| `src/renderer.rs` | N pieces with arbitrary destination rects. Clears to black each frame, and caches the stretch shader's views. |
| `src/cursor_portal.rs` | N-piece `PortalSplit` and `resolve_move`. The cursor wall applies to every hidden display. |
| `src/window_rescue.rs` | Supports several hidden displays. A rescued window goes to the piece under its centre. |

**Resolutions and driver (Phase B)**

| File | What changed |
| --- | --- |
| `src/modes.rs` (new) | Parsec built-in mode list, required modes per config, a 5-slot custom-mode planner, fallback modes, registry read/write, and the elevated `--register-modes` helper launched through UAC. |
| `src/driver.rs` (new) | Driver status (open the device, read its version). Downloads the installer over WinHTTP, checks it with WinVerifyTrust (signer must be "Parsec Cloud, Inc."), and launches it. Also holds the download/project/support links. |
| `examples/verify_probe.rs` (new) | Probe for the signature check. |

**UI and integration (Phases C and D)**

| File | What changed |
| --- | --- |
| `src/ui/*` (new, eframe 0.36.2 with glow) | `--settings` window with: a scaled layout preview; a splits panel with the detected-display dropdown, layout, ratios, rotate, main piece and refresh rate; a piece table showing built-in / registered / custom slot / fallback; driver and custom-slot panels; a hotkey recorder; general options; Save & apply; and a status line. |
| `src/tray.rs` | Rewritten. Menu: Settings…, one item per split, Start/Stop all, Cycle pieces, Start with Windows, Exit. Handles registered-message IPC, writes `status.json`, opens Settings on first run, and opens Settings on a second launch. |
| `src/ipc.rs` (new) | Registered messages and the `status.json` types. |
| `src/hotkeys.rs` (new) | Parsing, formatting and registration through `RegisterHotKey` with `MOD_NOREPEAT`. Reports "in use" (error 1409). |
| `src/autosplit.rs` (new) | Pure `decide()` for auto-split on connect. A manual stop pauses auto-start until the display is replugged. |
| `src/main.rs` | Modes `--settings`, `--register-modes`, `--cli`, `--seconds`. Single-instance handling per mode. `--cli` always logs to the console. |

**Packaging (Phase E)**
- `LICENSE` (MIT).
- `.github/workflows/ci.yml`: fmt, clippy, test and release build.
- `.github/workflows/release.yml`: on a `v*` tag, checks the tag matches the version, then builds the zip and SHA-256 and runs `gh release create`.
- `README.md` rewritten: use cases, comparison with snapping tools, driver install, examples table, FAQ, uninstall.
- `Cargo.toml` is version 0.2.0 with a new description.

### Deviations from the plan
- **Parking:** the layout planner prefers real non-split displays as parking neighbours and falls back to virtual pieces, so the FlipGo keeps its v0.1 position above the Asus.
- **Frame events:** one per split rather than one shared event, because auto-reset events can't be shared between renderers.
- **Worker status:** posted with `WM_APP+10` instead of a registered message.
- **Second tray launch:** opens the settings window in that process instead of messaging the tray.
- **Tray menu:** "Open config" and "Reload config" were removed, as the plan specified.
- **Fallback mode choice:** the nearest larger built-in mode by area (1720×1440 maps to 2256×1504), not by matching aspect ratio.
- **Built-in sizes and refresh:** only 60 Hz counts as built-in, so other refresh rates use a custom slot.

### Verification
- `cargo build`, `cargo build --release`, `cargo build --examples`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` all pass.
- `cargo test`: 103 passed, 0 failed (autosplit 13, hotkeys 6, ipc 1, ui 12, config 5, geometry 13, layout 13, cursor_portal 11, displays 5, modes 12, driver 5, window_rescue 4, vdd 2, autostart 1).
- **Regression run (orchestrator):** `--seconds 15` with the converted v0.1 config gave FlipGo pieces at (-1920,0)/(-1920,1080) and parked the FlipGo at (1520,-2160), the same as v0.1. Startup took about 4 s, against 10 s before. Stop restored the layout.
- **Signature probe:** the genuine Parsec installer verifies as Parsec Cloud, Inc.; unsigned files are rejected. A WinHTTP download of the installer matched the known SHA-256.
- **Settings window (implementer, temp APPDATA):** opens and lists displays, the preview is to scale, and the piece table and driver panel show correct values.
- **User tests (release build):** confirmed "it works, did not find any regression".
- **GitHub Actions tags:** `actions/checkout@v4.2.2` and `actions/cache@v4.2.0` checked against the GitHub API.
- **Not run:**
  - The GitHub workflows themselves. CI runs on the first push; release needs a `v0.2.0` tag.
  - Download & install of the driver up to launching the installer, which would reinstall the driver.

### Follow-ups and known limitations
- About one frame of latency; DRM video shows black; no HDR and no touch.
- At most 8 virtual displays and 5 custom sizes.
- The fallback mode could prefer a matching aspect ratio.
- `tile_virtual` fallback: the renderer, portal and rescue still use the real piece sizes, and a larger fallback could overlap neighbouring displays.
- The release exe grew from 2.4 MB to 9.5 MB because of eframe.

### Commit status
Committed on `VDD-02_generic_split_ui`. Not pushed, no PR.
