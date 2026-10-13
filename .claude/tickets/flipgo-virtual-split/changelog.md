# Changelog: flipgo-virtual-split

## 2026-10-13: FlipGo virtual split display, v0.1.0

Summary: a working tray app turns the FlipGo Lite's single tall input into two real 1920x1080 Windows monitors. The user confirmed it end to end.

### Repo `virtual-desktop-display` (branch `flipgo-virtual-split`, 5 commits: 652911f, 36af7f6, b0ea6b9, 1dd31d2, 4e8a36d)

| File | What it does |
|---|---|
| `Cargo.toml`, `build.rs`, `res/app.manifest`, `res/app.rc`, `res/icon.ico` | Rust crate (`windows` 0.62.2). Release exe has the GUI subsystem, PerMonitorV2 manifest and tray icon. |
| `src/vdd/parsec.rs`, `src/vdd/mod.rs` | parsec-vdd IOCTL client (add/remove/update/version) with a 50 ms keep-alive thread. Displays vanish if the app dies. |
| `src/displays.rs` | CCD enumeration; finds the physical display by name. New virtual targets are found via QDC_ALL_PATHS and activated: wait, then EXTEND, then manual path injection. Sets modes with DM_POSITION. `compute_layout` places the virtuals where the FlipGo was and parks the hidden FlipGo **above** the primary. Layouts are applied without SDC_SAVE_TO_DATABASE; the original is restored. |
| `src/capture.rs` | Windows.Graphics.Capture per virtual monitor (cursor included, best-effort borderless). Frames are copied to textures we own. |
| `src/renderer.rs` | Topmost no-activate window on the physical display with a flip swapchain. Uses CopySubresourceRegion, or a stretch shader if the sizes differ. Upper/lower is decided by layout position. |
| `src/cursor_portal.rs` | `WH_MOUSE_LL`. The hidden display acts as a wall; a cursor already inside it is mapped onto the matching panel (`resolve_move`). |
| `src/window_rescue.rs` | WinEvent hooks move windows off the hidden display onto the top virtual display, keeping their maximized state. |
| `src/session.rs` | Start/stop orchestration. Start waits for the layout to settle and retries capture. Stop removes the virtuals, then restores the layout. `set_swap` re-lays out the two virtual displays. |
| `src/tray.rs`, `src/autostart.rs`, `src/config.rs`, `src/main.rs` | Tray menu (start/stop, swap, open/reload config, start with Windows, exit), HKCU Run entry, TOML config in `%APPDATA%`, single instance, daily log file, `--cli` / `--seconds N`. |
| `examples/vdd_probe.rs`, `examples/capture_probe.rs` | Spike probes. |
| `README.md` | Setup, usage, config and known quirks. |

### Deviations from the plan
- **Activation:** the plan assumed Windows auto-activates new virtual targets. It didn't after the topology database was polluted, so explicit activation was added.
- **Capture start:** needed a settle/retry loop after the layout change, because the first attempt returns E_INVALIDARG.
- **Cursor portal → wall:** the planned "portal" teleport confused the user, so entering the hidden display is now a wall and mapping only applies when the cursor is already inside it.
- **Swap:** it first swapped only the rendering. It now swaps the Windows layout positions, which is what the user expected.
- **Hidden display position:** moved from below the primary to above it, so the cursor sprite no longer shows on the panel at the wall.
- **Window rescue:** always targets the top virtual display.

### Verification
- `cargo build`, `cargo build --release`, `cargo clippy --all-targets -- -D warnings`: pass.
- `cargo test`: 19 passed, 0 failed.
- `vdd_probe` as non-admin: pass.
- Full session with the user watching: two independent panels; YouTube fullscreen, maximize and Win+arrows per panel; cursor moves correctly; swap works. User confirmed: "all works".
- Crash safety (force kill): virtuals gone and layout restored within 6 s.
- Tray-mode file logging: verified by the implementer with a temp APPDATA.
- **Not run:** sleep/resume, unplugging the FlipGo during a split, lock/unlock and UAC with the cursor wall, autostart at real logon, stretch path (non-2x physical mode), second-instance message box.

### Known limitations and follow-ups
- About 1 frame of capture latency; DRM video shows black; no touch.
- Startup takes about 10 s: parsec ADD takes about 3 s per display, EXTEND wastes about 4 s on this machine. A possible fix is to go straight to manual injection.
- Settings → Identify draws the hidden display's "2" over the bottom panel's corner (cosmetic).
- The layout is FlipGo-specific: 2 stacked halves, primary-relative placement.

### Commit status
Committed on `flipgo-virtual-split` (not pushed, no PR).
