//! The two application root slots a Nest boots from, root0 and root1.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Slot {
    Root0,
    Root1,
}

impl Slot {
    pub fn number(self) -> u8 {
        self as u8
    }

    pub fn parse(name: &str) -> Option<Slot> {
        match name {
            "root0" => Some(Slot::Root0),
            "root1" => Some(Slot::Root1),
            _ => None,
        }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "root{}", self.number())
    }
}
