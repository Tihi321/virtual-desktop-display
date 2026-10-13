//! Auto-split: start / stop splits when their displays connect or disappear.
//!
//! The tray debounces display / device change messages and calls `decide`
//! with the running split set and which configured displays are present now.
//! `decide` is pure (all state is in `AutoState`).
//!
//! A split is *armed* when it may be started automatically:
//! * at launch when `start_on_launch` is set,
//! * when its display connects and `auto_on_connect` is set,
//! * when the user starts splits explicitly (Start all, hotkey, menu).
//!
//! A display disappearing disarms its split. A manual stop disarms everything,
//! so a manual stop is only overridden after the display was unplugged and
//! plugged in again.
use crate::config::Config;
use crate::displays::{self, TargetInfo};
use crate::session;

/// What the tray should do after a display change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Stop the session.
    Stop,
    /// (Re)start the session with exactly these splits (config indices).
    Restart(Vec<usize>),
}

#[derive(Debug, Clone, Default)]
pub struct AutoState {
    /// Presence at the last evaluation, per split.
    present: Vec<bool>,
    armed: Vec<bool>,
    /// Presence when the last start failed: no retry until it changes.
    failed_for: Option<Vec<bool>>,
}

impl AutoState {
    /// Initial state. Splits are armed only by `start_on_launch`; displays
    /// that are already connected at launch do not count as "just connected".
    pub fn new(cfg: &Config, present: &[bool], start_on_launch: bool) -> Self {
        let mut s = Self {
            present: fit(present, cfg.splits.len(), false),
            armed: vec![false; cfg.splits.len()],
            failed_for: None,
        };
        if start_on_launch {
            s.arm_all(cfg);
        }
        s
    }

    /// Arm every enabled split (explicit start).
    pub fn arm_all(&mut self, cfg: &Config) {
        self.armed = fit(&self.armed, cfg.splits.len(), false);
        for (a, sc) in self.armed.iter_mut().zip(&cfg.splits) {
            *a = sc.enabled;
        }
        self.failed_for = None;
    }

    pub fn arm(&mut self, index: usize) {
        if let Some(a) = self.armed.get_mut(index) {
            *a = true;
        }
        self.failed_for = None;
    }

    /// Manual stop: nothing restarts until a display is replugged.
    pub fn disarm_all(&mut self) {
        self.armed.iter_mut().for_each(|a| *a = false);
    }

    /// The last start failed: do not retry until the presence changes.
    pub fn note_failure(&mut self) {
        self.failed_for = Some(self.present.clone());
    }

    pub fn is_armed(&self, index: usize) -> bool {
        self.armed.get(index).copied().unwrap_or(false)
    }

    /// Splits that should be running: enabled, display present, armed.
    pub fn desired(&self, cfg: &Config, present: &[bool]) -> Vec<usize> {
        (0..cfg.splits.len())
            .filter(|&i| {
                cfg.splits[i].enabled
                    && present.get(i).copied().unwrap_or(false)
                    && self.is_armed(i)
            })
            .collect()
    }
}

fn fit(v: &[bool], n: usize, fill: bool) -> Vec<bool> {
    let mut o = v.to_vec();
    o.resize(n, fill);
    o
}

/// Decide what to do. `running`: config indices of the running splits (empty
/// when stopped); `present`: per split, its display is connected now.
pub fn decide(st: &mut AutoState, running: &[usize], present: &[bool], cfg: &Config) -> Action {
    let n = cfg.splits.len();
    let present = fit(present, n, false);
    st.present = fit(&st.present, n, false);
    st.armed = fit(&st.armed, n, false);

    for (i, sc) in cfg.splits.iter().enumerate() {
        match (st.present[i], present[i]) {
            // connected
            (false, true) => {
                if sc.auto_on_connect && sc.enabled {
                    st.armed[i] = true;
                }
            }
            // disconnected
            (true, false) => st.armed[i] = false,
            _ => {}
        }
    }
    st.present = present.clone();
    if st.failed_for.as_ref().is_some_and(|f| *f != present) {
        st.failed_for = None;
    }

    let desired = st.desired(cfg, &present);
    let mut run: Vec<usize> = running.to_vec();
    run.sort_unstable();
    run.dedup();
    if run == desired {
        return Action::Nothing;
    }
    if desired.is_empty() {
        return if run.is_empty() {
            Action::Nothing
        } else {
            Action::Stop
        };
    }
    if run.is_empty() && st.failed_for.is_some() {
        return Action::Nothing;
    }
    Action::Restart(desired)
}

/// Which configured splits have a connected display (virtual displays never
/// count; a display is claimed by at most one split). Disabled splits are
/// included so that their presence transitions are tracked too.
pub fn presence(cfg: &Config, targets: &[TargetInfo]) -> Vec<bool> {
    let mut all = cfg.clone();
    all.splits.iter_mut().for_each(|s| s.enabled = true);
    let resolved = session::resolve_all(&all, targets);
    (0..cfg.splits.len())
        .map(|i| resolved.iter().any(|r| r.index == i))
        .collect()
}

/// Presence from the live display topology.
pub fn current_presence(cfg: &Config) -> Vec<bool> {
    match displays::enumerate() {
        Ok(t) => presence(cfg, &t),
        Err(e) => {
            tracing::warn!("enumerate displays failed: {e:#}");
            vec![false; cfg.splits.len()]
        }
    }
}

/// A copy of `cfg` in which only the splits in `set` stay enabled (the worker
/// starts every enabled split whose display is present).
pub fn config_for(cfg: &Config, set: &[usize]) -> Config {
    let mut c = cfg.clone();
    for (i, s) in c.splits.iter_mut().enumerate() {
        s.enabled = s.enabled && set.contains(&i);
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SplitConfig;
    use crate::displays::{Rect, TargetKey};

    fn cfg(auto: &[bool]) -> Config {
        Config {
            splits: auto
                .iter()
                .enumerate()
                .map(|(i, a)| SplitConfig {
                    name: format!("s{i}"),
                    display_id: format!("ID{i}"),
                    auto_on_connect: *a,
                    ..SplitConfig::default()
                })
                .collect(),
            ..Config::default()
        }
    }

    #[test]
    fn connect_with_auto_starts() {
        let c = cfg(&[true]);
        let mut st = AutoState::new(&c, &[false], false);
        assert_eq!(decide(&mut st, &[], &[false], &c), Action::Nothing);
        assert_eq!(decide(&mut st, &[], &[true], &c), Action::Restart(vec![0]));
        // now running: stable
        assert_eq!(decide(&mut st, &[0], &[true], &c), Action::Nothing);
    }

    #[test]
    fn connect_without_auto_does_nothing() {
        let c = cfg(&[false]);
        let mut st = AutoState::new(&c, &[false], false);
        assert_eq!(decide(&mut st, &[], &[true], &c), Action::Nothing);
    }

    #[test]
    fn present_at_launch_not_started_without_start_on_launch() {
        let c = cfg(&[true]);
        let mut st = AutoState::new(&c, &[true], false);
        assert_eq!(decide(&mut st, &[], &[true], &c), Action::Nothing);
        // but after an unplug / replug it starts
        assert_eq!(decide(&mut st, &[], &[false], &c), Action::Nothing);
        assert_eq!(decide(&mut st, &[], &[true], &c), Action::Restart(vec![0]));
    }

    #[test]
    fn start_on_launch_arms_enabled() {
        let mut c = cfg(&[false, false]);
        c.splits[1].enabled = false;
        let mut st = AutoState::new(&c, &[true, true], true);
        assert_eq!(st.desired(&c, &[true, true]), vec![0]);
        assert_eq!(
            decide(&mut st, &[], &[true, true], &c),
            Action::Restart(vec![0])
        );
    }

    #[test]
    fn disconnect_stops_or_shrinks() {
        let c = cfg(&[true, true]);
        let mut st = AutoState::new(&c, &[true, true], true);
        assert_eq!(decide(&mut st, &[0, 1], &[true, true], &c), Action::Nothing);
        assert_eq!(
            decide(&mut st, &[0, 1], &[true, false], &c),
            Action::Restart(vec![0])
        );
        assert_eq!(decide(&mut st, &[0], &[false, false], &c), Action::Stop);
        assert_eq!(decide(&mut st, &[], &[false, false], &c), Action::Nothing);
    }

    #[test]
    fn connect_adds_to_running_set() {
        let c = cfg(&[true, true]);
        let mut st = AutoState::new(&c, &[true, false], true);
        assert_eq!(decide(&mut st, &[0], &[true, false], &c), Action::Nothing);
        assert_eq!(
            decide(&mut st, &[0], &[true, true], &c),
            Action::Restart(vec![0, 1])
        );
    }

    #[test]
    fn manual_stop_pauses_until_replug() {
        let c = cfg(&[true]);
        let mut st = AutoState::new(&c, &[true], true);
        assert_eq!(decide(&mut st, &[0], &[true], &c), Action::Nothing);
        st.disarm_all(); // user stopped
        assert_eq!(decide(&mut st, &[], &[true], &c), Action::Nothing);
        assert_eq!(decide(&mut st, &[], &[true], &c), Action::Nothing);
        assert_eq!(decide(&mut st, &[], &[false], &c), Action::Nothing);
        assert_eq!(decide(&mut st, &[], &[true], &c), Action::Restart(vec![0]));
    }

    #[test]
    fn manual_start_overrides_pause_and_non_auto() {
        let c = cfg(&[false]);
        let mut st = AutoState::new(&c, &[true], false);
        st.arm_all(&c);
        assert_eq!(decide(&mut st, &[], &[true], &c), Action::Restart(vec![0]));
    }

    #[test]
    fn failed_start_is_not_retried_until_presence_changes() {
        let c = cfg(&[true]);
        let mut st = AutoState::new(&c, &[false], false);
        assert_eq!(decide(&mut st, &[], &[true], &c), Action::Restart(vec![0]));
        st.note_failure();
        assert_eq!(decide(&mut st, &[], &[true], &c), Action::Nothing);
        assert_eq!(decide(&mut st, &[], &[true], &c), Action::Nothing);
        assert_eq!(decide(&mut st, &[], &[false], &c), Action::Nothing);
        assert_eq!(decide(&mut st, &[], &[true], &c), Action::Restart(vec![0]));
    }

    #[test]
    fn disabled_split_never_desired() {
        let mut c = cfg(&[true]);
        c.splits[0].enabled = false;
        let mut st = AutoState::new(&c, &[false], true);
        assert_eq!(decide(&mut st, &[], &[true], &c), Action::Nothing);
        // running a split that got disabled -> stop
        assert_eq!(decide(&mut st, &[0], &[true], &c), Action::Stop);
    }

    #[test]
    fn config_resize_is_tolerated() {
        let c = cfg(&[true]);
        let mut st = AutoState::new(&c, &[true], true);
        let c2 = cfg(&[true, true]);
        assert_eq!(decide(&mut st, &[0], &[true, false], &c2), Action::Nothing);
    }

    fn target(id: u32, name: &str, path: &str) -> TargetInfo {
        TargetInfo {
            key: TargetKey {
                luid_low: 1,
                luid_high: 0,
                target_id: id,
            },
            friendly_name: name.into(),
            device_path: path.into(),
            gdi_name: String::new(),
            rect: Rect::new(0, 0, 1920, 1080),
            hmonitor: 0,
            primary: false,
        }
    }

    #[test]
    fn presence_resolves_and_claims_once() {
        let mut c = cfg(&[true, true, true]);
        c.splits[2].display_id = "ID0".into(); // same display as split 0
        let t = vec![
            target(1, "A", r"\\?\DISPLAY#ID0#x"),
            target(2, "V", r"\\?\DISPLAY#PSCCDD0#x"),
        ];
        assert_eq!(presence(&c, &t), vec![true, false, false]);
    }

    #[test]
    fn config_for_keeps_only_set() {
        let c = cfg(&[true, true, true]);
        let d = config_for(&c, &[1]);
        assert_eq!(
            d.splits.iter().map(|s| s.enabled).collect::<Vec<_>>(),
            vec![false, true, false]
        );
    }
}
