//! Orchestrates the whole pipeline.
//!
//! Start: vdd add -> wait for targets and make sure they are active -> set
//! modes -> layout -> re-enumerate -> capture -> renderer -> cursor portal +
//! window rescue.
//!
//! Stop (reverse): window rescue, cursor portal, renderer, captures, then
//! REMOVE the virtual displays, wait until they are gone, and only then
//! restore the original layout. Restoring while the virtual displays are still
//! attached would make Windows remember "virtuals inactive" for the 4-monitor
//! topology, so the original layout is applied to the original 2-monitor set.
use crate::capture::{init_winrt, FrameEvent, MonitorCapture};
use crate::config::Config;
use crate::cursor_portal::CursorPortal;
use crate::displays::{self, SavedLayout, TargetInfo, TargetKey};
use crate::renderer::{create_device, RenderConfig, Renderer};
use crate::vdd::parsec::{ParsecVdd, VDD_DISPLAY_NAME};
use crate::vdd::VirtualDisplays;
use crate::window_rescue::WindowRescue;
use anyhow::{anyhow, Context, Result};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

pub struct Session {
    rescue: Option<WindowRescue>,
    portal: Option<CursorPortal>,
    renderer: Option<Renderer>,
    captures: Vec<Arc<MonitorCapture>>,
    saved_layout: Option<SavedLayout>,
    vdd: Option<ParsecVdd>,
    pub swap: Arc<AtomicBool>,
    pub v_top: TargetInfo,
    pub v_bot: TargetInfo,
    pub physical: TargetInfo,
    /// Virtual targets that were activated (to wait for their removal).
    virtual_keys: Vec<TargetKey>,
}

impl Session {
    /// `swap` is shared with the renderer and cursor portal and may be
    /// toggled at any time.
    pub fn start(cfg: &Config, swap: Arc<AtomicBool>) -> Result<Self> {
        init_winrt();
        let snap = displays::snapshot().context("enumerate displays")?;
        let before = snap.active.clone();
        for t in &before {
            tracing::info!("before: {} {} {:?}", t.friendly_name, t.gdi_name, t.rect);
        }
        let physical = displays::find_physical(&before, &cfg.physical_match)
            .ok_or_else(|| anyhow!("no display matching {:?} found", cfg.physical_match))?
            .clone();
        let primary = before
            .iter()
            .find(|t| t.primary)
            .ok_or_else(|| anyhow!("no primary display"))?
            .clone();
        let has_virtual = before
            .iter()
            .any(|t| t.friendly_name.contains(VDD_DISPLAY_NAME));
        if !has_virtual && displays::looks_like_our_layout(primary.rect, physical.rect) {
            tracing::warn!(
                "physical display is at {:?}, where our layout puts it, but no virtual displays exist: \
                 a previous run probably left its layout behind (open Display settings to fix)",
                physical.rect
            );
        }
        let saved = displays::save_layout().context("save layout")?;
        tracing::info!(
            "saved original layout; physical = {} {:?}",
            physical.friendly_name,
            physical.rect
        );

        tracing::info!("step 1: adding 2 virtual displays");
        let mut vdd = ParsecVdd::open().context("open parsec-vdd")?;
        tracing::info!("parsec-vdd minor version: {:?}", vdd.version().ok());

        // From here on, a failure must still unwind cleanly: build a partial
        // Session and let `Drop` do the cleanup.
        let mut s = Session {
            rescue: None,
            portal: None,
            renderer: None,
            captures: Vec::new(),
            saved_layout: Some(saved),
            vdd: None,
            swap: swap.clone(),
            v_top: physical.clone(),
            v_bot: physical.clone(),
            physical: physical.clone(),
            virtual_keys: Vec::new(),
        };
        let added = vdd.add(2);
        s.vdd = Some(vdd);
        added?;

        tracing::info!("step 2: waiting for new targets and activating them");
        let new = displays::bring_up_new_targets(&snap, 2, Duration::from_secs(8))?;
        s.virtual_keys = new.iter().map(|t| t.key).collect();
        for t in &new {
            tracing::info!(
                "new target: {} {} {}",
                t.friendly_name,
                t.gdi_name,
                t.device_path
            );
        }
        let (top, bot) = (new[0].clone(), new[1].clone());

        tracing::info!(
            "step 3: setting modes {}x{}@{}",
            cfg.width,
            cfg.height,
            cfg.refresh_hz
        );
        for t in [&top, &bot] {
            displays::set_mode(&t.gdi_name, cfg.width, cfg.height, cfg.refresh_hz)?;
        }

        tracing::info!("step 4: applying layout");
        let layout = displays::compute_layout(primary.rect, physical.rect, cfg.height as i32);
        tracing::info!("layout: {layout:?}");
        displays::apply_layout(&layout, top.key, bot.key, physical.key)?;

        tracing::info!("step 5: re-enumerating");
        let after = displays::enumerate()?;
        let find = |k| {
            after
                .iter()
                .find(|t| t.key == k)
                .cloned()
                .ok_or_else(|| anyhow!("target {k:?} vanished after layout"))
        };
        s.v_top = find(top.key)?;
        s.v_bot = find(bot.key)?;
        s.physical = find(physical.key)?;
        tracing::info!(
            "v_top {:?}, v_bot {:?}, physical {:?}",
            s.v_top.rect,
            s.v_bot.rect,
            s.physical.rect
        );

        tracing::info!("step 6: capture + renderer");
        let d3d = create_device()?;
        let event = FrameEvent::new()?;
        let cap_top = Arc::new(MonitorCapture::start(
            &d3d.device,
            &d3d.context,
            s.v_top.hmonitor,
            event.clone(),
        )?);
        s.captures.push(cap_top.clone());
        let cap_bot = Arc::new(MonitorCapture::start(
            &d3d.device,
            &d3d.context,
            s.v_bot.hmonitor,
            event.clone(),
        )?);
        s.captures.push(cap_bot.clone());
        let renderer = Renderer::start(
            &d3d,
            RenderConfig {
                physical: s.physical.rect,
                swap: swap.clone(),
            },
            cap_top,
            cap_bot,
            event,
        )?;
        let own_hwnd = renderer.hwnd();
        s.renderer = Some(renderer);

        tracing::info!("step 7: cursor portal + window rescue");
        s.portal = Some(CursorPortal::start(
            s.physical.rect,
            s.v_top.rect,
            s.v_bot.rect,
            swap,
        )?);
        s.rescue = Some(WindowRescue::start(
            s.physical.hmonitor,
            s.physical.rect,
            s.v_top.rect,
            own_hwnd,
        )?);
        tracing::info!("session running");
        Ok(s)
    }

    /// True while the physical display and both virtual targets are still active.
    pub fn is_healthy(&self) -> bool {
        let keys = [self.physical.key, self.v_top.key, self.v_bot.key];
        displays::all_present(&keys).unwrap_or(false)
    }

    /// Stop everything in reverse order and restore the original layout.
    pub fn stop(&mut self) {
        if let Some(mut r) = self.rescue.take() {
            tracing::info!("stopping window rescue");
            r.stop();
        }
        if let Some(mut p) = self.portal.take() {
            tracing::info!("stopping cursor portal");
            p.stop();
        }
        if let Some(mut r) = self.renderer.take() {
            tracing::info!("stopping renderer");
            r.stop();
        }
        if !self.captures.is_empty() {
            tracing::info!("stopping captures");
            self.captures.clear();
        }
        // Remove the virtual displays BEFORE restoring the layout.
        if let Some(mut v) = self.vdd.take() {
            tracing::info!("removing virtual displays");
            v.remove_all();
            drop(v);
            if !self.virtual_keys.is_empty() {
                let gone = displays::wait_targets_gone(&self.virtual_keys, Duration::from_secs(5));
                tracing::info!("virtual targets gone: {gone}");
            }
        }
        if let Some(l) = self.saved_layout.take() {
            tracing::info!("restoring original layout");
            if let Err(e) = displays::restore_layout(&l) {
                tracing::error!("restore layout failed: {e}");
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}
