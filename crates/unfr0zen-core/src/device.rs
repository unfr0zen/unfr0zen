//! The Nest generations unfr0zen supports.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    /// Nest gen 1, TI AM3703, JFFS2 root slots.
    Diamond,
    /// Nest gen 3, i.MX6SoloX, UBIFS root slots.
    Diamond3,
    /// Nest gen 2, TI AM3703, JFFS2 root slots.
    J49,
    /// Nest Thermostat E, i.MX6UL; detection only.
    Onyx,
}

impl Device {
    pub fn name(self) -> &'static str {
        match self {
            Device::Diamond => "diamond",
            Device::Diamond3 => "diamond3",
            Device::J49 => "j49",
            Device::Onyx => "onyx",
        }
    }

    /// The generation, as Nest's U-Boot tells the boards apart: `nlmodel` is
    /// the family, a dash, the product and an optional dotted revision,
    /// `Diamond-1.N` on a gen 1, `Display-2.N` on a gen 2, and `Display-3.N`
    /// or `Display-5.N` on a gen 3. None for any other model.
    pub fn from_nlmodel(model: &str) -> Option<Device> {
        let (family, rest) = model.rsplit_once('-')?;
        match (family, rest.split('.').next()?) {
            ("Diamond", "1") => Some(Device::Diamond),
            ("Display", "2") => Some(Device::J49),
            ("Display", "3" | "5") => Some(Device::Diamond3),
            _ => None,
        }
    }

    /// Its generation and name, as a person knows it.
    pub fn description(self) -> &'static str {
        match self {
            Device::Diamond => "Nest gen 1 (diamond)",
            Device::Diamond3 => "Nest gen 3 (diamond3)",
            Device::J49 => "Nest gen 2 (j49)",
            Device::Onyx => "Nest Thermostat E (onyx)",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_nlmodel_maps_every_generation() {
        assert_eq!(Device::from_nlmodel("Display-3.4"), Some(Device::Diamond3));
        assert_eq!(Device::from_nlmodel("Display-5.1"), Some(Device::Diamond3));
        assert_eq!(Device::from_nlmodel("Display-2.8"), Some(Device::J49));
        assert_eq!(Device::from_nlmodel("Display-2"), Some(Device::J49));
        assert_eq!(Device::from_nlmodel("Diamond-1.10"), Some(Device::Diamond));
        for other in ["Onyx-1", "", "Diamond", "Display-4.1", "Display-31.0", "Diamond-1.x-2"] {
            assert_eq!(Device::from_nlmodel(other), None, "{other}");
        }
    }
}
