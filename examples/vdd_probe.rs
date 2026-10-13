//! Phase 0 probe: add 2 parsec-vdd displays (run as a NON-admin user), keep
//! them alive 30 s, print before/after targets, then remove them.
use std::time::Duration;
use vdd_app::displays::{self, TargetInfo};
use vdd_app::vdd::parsec::ParsecVdd;
use vdd_app::vdd::VirtualDisplays;

fn dump(title: &str, v: &[TargetInfo]) {
    println!("== {title} ({} targets)", v.len());
    for t in v {
        println!(
            "  name={:?} gdi={} rect={:?} primary={} hmon={:#x}\n    path={}\n    key={:?}",
            t.friendly_name, t.gdi_name, t.rect, t.primary, t.hmonitor, t.device_path, t.key
        );
    }
}

fn main() -> anyhow::Result<()> {
    vdd_app::set_dpi_aware();
    let snap = displays::snapshot()?;
    let before = snap.active.clone();
    dump("before", &before);
    let mut vdd = ParsecVdd::open()?;
    println!("driver version (minor): {:?}", vdd.version());
    let idx = vdd.add(2)?;
    println!("added indices {idx:?}");
    let new = displays::bring_up_new_targets(&snap, 2, Duration::from_secs(8))?;
    dump("new targets", &new);
    dump("after", &displays::enumerate()?);
    println!("keeping alive for 30 s...");
    std::thread::sleep(Duration::from_secs(30));
    vdd.remove_all();
    drop(vdd);
    let keys: Vec<_> = new.iter().map(|t| t.key).collect();
    println!(
        "gone: {}",
        displays::wait_targets_gone(&keys, Duration::from_secs(5))
    );
    dump("after remove", &displays::enumerate()?);
    Ok(())
}
