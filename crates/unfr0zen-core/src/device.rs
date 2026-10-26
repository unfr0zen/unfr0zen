//! The Nest generations unfr0zen supports.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    /// Nest gen 1, TI AM3703, JFFS2 root slots.
    Diamond,
    /// Nest gen 3, i.MX6SoloX, UBIFS root slots.
    Diamond3,
    /// Nest gen 2, TI AM3703, JFFS2 root slots.
    J49,
}

impl Device {
    pub fn name(self) -> &'static str {
        match self {
            Device::Diamond => "diamond",
            Device::Diamond3 => "diamond3",
            Device::J49 => "j49",
        }
    }

    /// Its generation and name, as a person knows it.
    pub fn description(self) -> &'static str {
        match self {
            Device::Diamond => "Nest gen 1 (diamond)",
            Device::Diamond3 => "Nest gen 3 (diamond3)",
            Device::J49 => "Nest gen 2 (j49)",
        }
    }
}
