//! Orchestrates the whole pipeline for ALL running splits at once.
//!
//! Start: vdd add (sum of all pieces) -> wait for the targets and make sure
//! they are active -> set a mode per piece -> layout (`layout::plan`) ->
//! re-enumerate and settle -> one capture per piece -> one renderer per split ->
//! one cursor portal + one window rescue for everything.
//!
//! Stop (reverse): window rescue, cursor portal, renderers, captures, then
//! REMOVE the virtual displays, wait until they are gone, and only then
//! restore the original layout. Restoring while the virtual displays are still
//! attached would make Windows remember "virtuals inactive" for the larger
//! topology, so the original layout is applied to the original monitor set.
use crate::capture::{init_winrt, FrameEvent, MonitorCapture};
use crate::config::Config;
use crate::cursor_portal::{CursorPortal, PortalSplit};
use crate::displays::{self, Rect, SavedLayout, TargetInfo, TargetKey};
use crate::geometry::{self, Spec};
use crate::layout::{self, DisplayGeom, SplitGeom};
use crate::renderer::{create_device, D3d, Renderer};
use crate::vdd::parsec::{ParsecVdd, VDD_MAX_DISPLAYS};
use crate::vdd::VirtualDisplays;
use crate::window_rescue::{RescueSplit, WindowRescue};
use anyhow::{anyhow, bail, Context, Result};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A configured split whose display is present.
#[derive(Debug, Clone)]
pub struct ResolvedSplit {
    /// Index into `Config::splits`.
    pub index: usize,
    pub physical: TargetInfo,
    /// Pieces relative to the physical display, in virtual display order.
    pub pieces_rel: Vec<Rect>,
    pub refresh: u32,
}

/// Resolve every enabled split of `cfg` whose display is among `targets`
/// (ParsecVDA displays are never matched; a display is used by one split only).
pub fn resolve_all(cfg: &Config, targets: &[TargetInfo]) -> Vec<ResolvedSplit> {
    let mut out: Vec<ResolvedSplit> = Vec::new();
    for (index, sc) in cfg.splits.iter().enumerate() {
        if !sc.enabled {
            continue;
        }
        let Some(t) = displays::resolve_split(targets, sc) else {
            tracing::info!("split {:?}: display not connected", sc.name);
            continue;
        };
        if t.rect.w <= 0 || t.rect.h <= 0 {
            tracing::warn!("split {:?}: display has no size", sc.name);
            continue;
        }
        if out.iter().any(|r| r.physical.key == t.key) {
            tracing::warn!(
                "split {:?}: display {} is already used by another split; skipped",
                sc.name,
                t.friendly_name
            );
            continue;
        }
        out.push(ResolvedSplit {
            index,
            physical: t.clone(),
            pieces_rel: geometry::pieces(t.rect.w, t.rect.h, &sc.spec()),
            refresh: sc.refresh_hz.max(1),
        });
    }
    out
}

/// Runtime state of one split.
struct SplitRt {
    index: usize,
    spec: Spec,
    /// Geometric cell that becomes the primary origin (primary display only).
    main_cell: usize,
    refresh: u32,
    /// The physical display as it was before the session.
    orig: TargetInfo,
    /// Virtual targets, per piece (virtual display order).
    virt: Vec<TargetKey>,
    /// The physical display after the last layout (fresh after every pipeline start).
    physical: TargetInfo,
}

impl SplitRt {
    fn pieces(&self) -> Vec<Rect> {
        geometry::pieces(self.orig.rect.w, self.orig.rect.h, &self.spec)
    }

    fn order(&self) -> Vec<usize> {
        geometry::repair_order(&self.spec.order, geometry::piece_count(&self.spec))
    }

    /// Rotate the piece order; false if there is nothing to rotate.
    fn rotate(&mut self) -> bool {
        let n = geometry::piece_count(&self.spec);
        if n < 2 {
            return false;
        }
        self.spec.order = geometry::rotate_order(&self.spec.order, n);
        true
    }
}

/// Planned final state of a target: position and (virtual displays only) size.
type Expected = (TargetKey, (i32, i32), Option<(i32, i32)>);

/// An active, settled split.
struct SettledSplit {
    physical: TargetInfo,
    virt: Vec<TargetInfo>,
    caps: Vec<Arc<MonitorCapture>>,
}

/// One attempt: re-enumerate, require every planned target to be active at its
/// planned position (virtual ones at their piece size) with valid HMONITORs,
/// then start all captures. Any failure (including CreateForMonitor
/// E_INVALIDARG on a not-yet-attached monitor) returns Err so the caller can
/// retry.
fn try_capture(
    splits: &[SplitRt],
    pieces: &[Vec<Rect>],
    expect: &[Expected],
    d3d: &D3d,
    events: &[Arc<FrameEvent>],
) -> Result<Vec<SettledSplit>> {
    let after = displays::enumerate()?;
    let find = |k: TargetKey| -> Result<TargetInfo> {
        let (_, pos, size) = expect
            .iter()
            .find(|(ek, _, _)| *ek == k)
            .ok_or_else(|| anyhow!("target {k:?} not in the layout plan"))?;
        let t = after
            .iter()
            .find(|t| t.key == k)
            .ok_or_else(|| anyhow!("target {k:?} not active yet"))?;
        if t.hmonitor == 0 {
            return Err(anyhow!("target {k:?} has no HMONITOR yet"));
        }
        if (t.rect.x, t.rect.y) != *pos || size.is_some_and(|sz| (t.rect.w, t.rect.h) != sz) {
            return Err(anyhow!("target {k:?} at {:?}, expected {pos:?}", t.rect));
        }
        Ok(t.clone())
    };
    // Everything else in the plan (unsplit real displays) must be in place too.
    for (k, pos, _) in expect {
        match after.iter().find(|t| t.key == *k) {
            Some(t) if (t.rect.x, t.rect.y) == *pos => {}
            Some(t) => return Err(anyhow!("target {k:?} at {:?}, expected {pos:?}", t.rect)),
            None => return Err(anyhow!("target {k:?} not active yet")),
        }
    }
    let mut out = Vec::new();
    for (si, s) in splits.iter().enumerate() {
        let physical = find(s.physical_key())?;
        let mut virt = Vec::new();
        let mut caps = Vec::new();
        for k in &s.virt {
            virt.push(find(*k)?);
        }
        for v in &virt {
            caps.push(Arc::new(MonitorCapture::start(
                &d3d.device,
                &d3d.context,
                v.hmonitor,
                events[si].clone(),
            )?));
        }
        debug_assert_eq!(pieces[si].len(), virt.len());
        out.push(SettledSplit {
            physical,
            virt,
            caps,
        });
    }
    Ok(out)
}

impl SplitRt {
    fn physical_key(&self) -> TargetKey {
        self.orig.key
    }
}

pub struct Session {
    rescue: Option<WindowRescue>,
    portal: Option<CursorPortal>,
    renderers: Vec<Renderer>,
    captures: Vec<Arc<MonitorCapture>>,
    saved_layout: Option<SavedLayout>,
    vdd: Option<ParsecVdd>,
    d3d: Option<D3d>,
    /// All real displays before the session (layout inputs).
    real: Vec<DisplayGeom>,
    splits: Vec<SplitRt>,
}

impl Session {
    /// Start all `splits` in one session. `cfg` supplies the geometry of each
    /// (by `ResolvedSplit::index`).
    pub fn start(cfg: &Config, splits: Vec<ResolvedSplit>) -> Result<Self> {
        init_winrt();
        if splits.is_empty() {
            bail!("no configured split matches a connected display");
        }
        let total: usize = splits.iter().map(|s| s.pieces_rel.len()).sum();
        if total == 0 {
            bail!("the splits have no pieces");
        }
        if total > VDD_MAX_DISPLAYS {
            bail!("the splits need {total} virtual displays; the driver supports at most {VDD_MAX_DISPLAYS}");
        }

        let snap = displays::snapshot().context("enumerate displays")?;
        let before = snap.active.clone();
        for t in &before {
            tracing::info!("before: {} {} {:?}", t.friendly_name, t.gdi_name, t.rect);
        }
        let real: Vec<DisplayGeom> = before
            .iter()
            .filter(|t| !t.is_virtual())
            .map(|t| DisplayGeom {
                key: t.key,
                rect: t.rect,
                primary: t.primary,
            })
            .collect();
        let mut rts = Vec::new();
        for rs in &splits {
            let sc = cfg
                .splits
                .get(rs.index)
                .ok_or_else(|| anyhow!("split index {} not in the config", rs.index))?;
            // Use the freshest physical info from the snapshot.
            let orig = before
                .iter()
                .find(|t| t.key == rs.physical.key)
                .ok_or_else(|| {
                    anyhow!("display {} is no longer active", rs.physical.friendly_name)
                })?
                .clone();
            rts.push(SplitRt {
                index: rs.index,
                spec: sc.spec(),
                main_cell: sc.main_piece,
                refresh: rs.refresh,
                physical: orig.clone(),
                orig,
                virt: Vec::new(),
            });
        }
        let saved = displays::save_layout().context("save layout")?;
        for r in &rts {
            tracing::info!(
                "split {}: {} {:?} -> {} pieces",
                r.index,
                r.orig.friendly_name,
                r.orig.rect,
                r.pieces().len()
            );
        }

        tracing::info!("step 1: adding {total} virtual displays");
        let mut vdd = ParsecVdd::open().context("open parsec-vdd")?;
        tracing::info!("parsec-vdd minor version: {:?}", vdd.version().ok());

        // From here on, a failure must still unwind cleanly: build a partial
        // Session and let `Drop` do the cleanup.
        let mut s = Session {
            rescue: None,
            portal: None,
            renderers: Vec::new(),
            captures: Vec::new(),
            saved_layout: Some(saved),
            vdd: None,
            d3d: None,
            real,
            splits: rts,
        };
        let added = vdd.add(total);
        s.vdd = Some(vdd);
        added?;

        tracing::info!("step 2: waiting for new targets and activating them");
        let new = displays::bring_up_new_targets(&snap, total, Duration::from_secs(10))?;
        for t in &new {
            tracing::info!(
                "new target: {} {} {}",
                t.friendly_name,
                t.gdi_name,
                t.device_path
            );
        }
        // Partition by ascending target id in split / piece order.
        let mut it = new.iter();
        for (rt, rs) in s.splits.iter_mut().zip(&splits) {
            for _ in 0..rs.pieces_rel.len() {
                let t = it.next().ok_or_else(|| anyhow!("not enough new targets"))?;
                rt.virt.push(t.key);
            }
        }

        s.d3d = Some(create_device()?);
        s.run_pipeline()?;
        tracing::info!(
            "session running: {} split(s), {total} virtual display(s)",
            s.splits.len()
        );
        Ok(s)
    }

    /// Modes + layout + settle + capture + renderers + portal + rescue, for the
    /// current piece orders. Everything is by POSITION: the display placed at a
    /// piece's offset is captured into that piece.
    fn run_pipeline(&mut self) -> Result<()> {
        let d3d = self.d3d.clone().ok_or_else(|| anyhow!("no D3D device"))?;
        let pieces: Vec<Vec<Rect>> = self.splits.iter().map(|s| s.pieces()).collect();

        tracing::info!("step 3: setting modes");
        let now = displays::enumerate()?;
        for (s, ps) in self.splits.iter().zip(&pieces) {
            for (k, p) in s.virt.iter().zip(ps) {
                let t = now
                    .iter()
                    .find(|t| t.key == *k)
                    .ok_or_else(|| anyhow!("virtual target {k:?} not active"))?;
                displays::set_mode(
                    &t.gdi_name,
                    p.w as u32,
                    p.h as u32,
                    s.refresh,
                    (t.rect.x, t.rect.y),
                )?;
            }
        }

        let geoms: Vec<SplitGeom> = self
            .splits
            .iter()
            .zip(&pieces)
            .map(|(s, ps)| {
                let n = ps.len();
                SplitGeom {
                    physical_key: s.orig.key,
                    physical_rect: s.orig.rect,
                    pieces_rel: ps.clone(),
                    is_primary: s.orig.primary,
                    main_piece: geometry::virtual_index_of_cell(&s.order(), s.main_cell.min(n - 1)),
                }
            })
            .collect();
        let plan = layout::plan(&self.real, &geoms);
        let virt_keys: Vec<Vec<TargetKey>> = self.splits.iter().map(|s| s.virt.clone()).collect();
        let positions = plan.positions(&virt_keys);
        tracing::info!("applying layout: {positions:?}");
        displays::apply_layout(&positions)?;

        // Expected final state: positions from the plan, sizes for virtual displays.
        let mut expect: Vec<Expected> = Vec::new();
        for ((keys, poss), ps) in virt_keys.iter().zip(&plan.virtual_positions).zip(&pieces) {
            for ((k, pos), p) in keys.iter().zip(poss).zip(ps) {
                expect.push((*k, *pos, Some((p.w, p.h))));
            }
        }
        expect.extend(plan.parked.iter().map(|(k, pos)| (*k, *pos, None)));
        expect.extend(plan.real.iter().map(|(k, pos)| (*k, *pos, None)));

        tracing::info!("waiting for the topology to settle, then capture");
        // One frame event per split: it is an auto-reset event, so renderers
        // must not share one.
        let events: Vec<Arc<FrameEvent>> = self
            .splits
            .iter()
            .map(|_| FrameEvent::new())
            .collect::<Result<_>>()?;
        let deadline = Instant::now() + Duration::from_secs(8);
        let settled = loop {
            match try_capture(&self.splits, &pieces, &expect, &d3d, &events) {
                Ok(v) => break v,
                Err(e) if Instant::now() < deadline => {
                    tracing::info!("not ready, retrying: {e:#}");
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => return Err(e),
            }
        };

        let mut portal_splits = Vec::new();
        let mut rescue_splits = Vec::new();
        let mut own_hwnds = Vec::new();
        for (((s, st), ps), ev) in self
            .splits
            .iter_mut()
            .zip(settled)
            .zip(&pieces)
            .zip(&events)
        {
            s.physical = st.physical;
            tracing::info!(
                "split {}: physical {:?}, virtual {:?}",
                s.index,
                s.physical.rect,
                st.virt.iter().map(|v| v.rect).collect::<Vec<_>>()
            );
            self.captures.extend(st.caps.iter().cloned());

            tracing::info!("starting renderer for split {}", s.index);
            let renderer = Renderer::start(
                &d3d,
                s.physical.rect,
                st.caps.iter().cloned().zip(ps.iter().copied()).collect(),
                ev.clone(),
            )?;
            own_hwnds.push(renderer.hwnd());
            self.renderers.push(renderer);

            let pieces_pair: Vec<(Rect, Rect)> = ps
                .iter()
                .copied()
                .zip(st.virt.iter().map(|v| v.rect))
                .collect();
            portal_splits.push(PortalSplit {
                physical: s.physical.rect,
                pieces: pieces_pair.clone(),
            });
            rescue_splits.push(RescueSplit {
                physical_hmon: s.physical.hmonitor,
                physical: s.physical.rect,
                pieces: pieces_pair,
            });
        }

        tracing::info!("starting cursor portal + window rescue");
        self.portal = Some(CursorPortal::start(portal_splits)?);
        self.rescue = Some(WindowRescue::start(rescue_splits, own_hwnds)?);
        Ok(())
    }

    /// Stop rescue, portal, renderers and captures (the vdd and its keep-alive keep running).
    fn stop_pipeline(&mut self) {
        if let Some(mut r) = self.rescue.take() {
            tracing::info!("stopping window rescue");
            r.stop();
        }
        if let Some(mut p) = self.portal.take() {
            tracing::info!("stopping cursor portal");
            p.stop();
        }
        if !self.renderers.is_empty() {
            tracing::info!("stopping renderers");
            for mut r in self.renderers.drain(..) {
                r.stop();
            }
        }
        if !self.captures.is_empty() {
            tracing::info!("stopping captures");
            self.captures.clear();
        }
    }

    /// Rebuild the pipeline after an order change. On failure the whole
    /// session is stopped cleanly (virtual displays removed, layout restored).
    fn rebuild(&mut self) -> Result<()> {
        self.stop_pipeline();
        if let Err(e) = self.run_pipeline() {
            self.stop();
            return Err(e);
        }
        Ok(())
    }

    /// Rotate the piece order of the split with config index `split_index`
    /// (a swap for two pieces) and rebuild the pipeline.
    pub fn cycle_order(&mut self, split_index: usize) -> Result<()> {
        let mut changed = false;
        for s in self.splits.iter_mut().filter(|s| s.index == split_index) {
            changed |= s.rotate();
        }
        if !changed {
            return Ok(());
        }
        tracing::info!("cycle split {split_index}: rebuilding the pipeline");
        self.rebuild()
    }

    /// Rotate the piece order of every running split with 2 or more pieces.
    pub fn cycle_all(&mut self) -> Result<()> {
        let mut changed = false;
        for s in self.splits.iter_mut() {
            changed |= s.rotate();
        }
        if !changed {
            return Ok(());
        }
        tracing::info!("cycle all splits: rebuilding the pipeline");
        self.rebuild()
    }

    /// Current piece order per running split: (config index, order).
    pub fn orders(&self) -> Vec<(usize, Vec<usize>)> {
        self.splits.iter().map(|s| (s.index, s.order())).collect()
    }

    /// Config indices of the running splits.
    pub fn split_indices(&self) -> Vec<usize> {
        self.splits.iter().map(|s| s.index).collect()
    }

    fn all_keys(&self) -> Vec<TargetKey> {
        let mut v = Vec::new();
        for s in &self.splits {
            v.push(s.orig.key);
            v.extend(s.virt.iter().copied());
        }
        v
    }

    /// True while every physical display and virtual target is still active.
    pub fn is_healthy(&self) -> bool {
        displays::all_present(&self.all_keys()).unwrap_or(false)
    }

    /// Stop everything in reverse order and restore the original layout.
    pub fn stop(&mut self) {
        self.stop_pipeline();
        // Remove the virtual displays BEFORE restoring the layout.
        if let Some(mut v) = self.vdd.take() {
            tracing::info!("removing virtual displays");
            v.remove_all();
            drop(v);
            let virt: Vec<TargetKey> = self
                .splits
                .iter()
                .flat_map(|s| s.virt.iter().copied())
                .collect();
            let gone = displays::wait_targets_gone(&virt, Duration::from_secs(5));
            tracing::info!("virtual targets gone: {gone}");
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
