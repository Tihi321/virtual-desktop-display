//! Virtual display backends.
pub mod parsec;

/// A source of virtual monitors. Displays live as long as the implementor does
/// (or until `remove_all`).
pub trait VirtualDisplays {
    /// Plug `n` virtual displays; returns driver-side indices.
    fn add(&mut self, n: usize) -> anyhow::Result<Vec<u32>>;
    /// Unplug everything this instance added.
    fn remove_all(&mut self);
}
