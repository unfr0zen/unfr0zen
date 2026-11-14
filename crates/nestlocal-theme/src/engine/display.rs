//! Where the round theme sits on the framebuffer, for each display the UI runs on.
//!
//! Themes draw an upright theme in their own design units (the bundled themes use
//! 480). The engine maps those onto the panel: scaled to the theme's real size,
//! moved to its place in the framebuffer and turned to the panel's mounting.

use std::f32::consts::FRAC_PI_2;

/// The panel and the theme on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    /// The framebuffer's size in pixels.
    pub fb_width: u32,
    pub fb_height: u32,
    /// The theme's diameter in pixels.
    pub theme: f32,
    /// The top-left corner of the theme's square, in framebuffer pixels.
    pub origin: (f32, f32),
    /// Clockwise quarter turns from the upright theme to the framebuffer.
    pub quarter_turns: u8,
}

impl Geometry {
    /// The display a framebuffer of this size belongs to. The gen 3's
    /// 480x640 panel is mounted on its side with the round theme in its top
    /// 480x480; any other panel is taken as upright with the theme centred.
    pub fn for_framebuffer(width: u32, height: u32) -> Geometry {
        match (width, height) {
            (480, 640) => Geometry { fb_width: width, fb_height: height, theme: 480.0, origin: (0.0, 0.0), quarter_turns: 1 },
            _ => {
                let theme = width.min(height) as f32;
                Geometry {
                    fb_width: width,
                    fb_height: height,
                    theme,
                    origin: ((width as f32 - theme) / 2.0, (height as f32 - theme) / 2.0),
                    quarter_turns: 0,
                }
            }
        }
    }

    /// The transform from a theme's upright design units to framebuffer
    /// pixels, as the steps a canvas applies in order.
    pub fn transform(&self, design: f32) -> Vec<Step> {
        let f = self.theme;
        let (x, y) = self.origin;
        let turn = match self.quarter_turns % 4 {
            0 => (0.0, 0.0),
            1 => (f, 0.0),
            2 => (f, f),
            _ => (0.0, f),
        };
        let mut steps = vec![Step::Translate(x + turn.0, y + turn.1)];
        if !self.quarter_turns.is_multiple_of(4) {
            steps.push(Step::Rotate(FRAC_PI_2 * f32::from(self.quarter_turns % 4)));
        }
        steps.push(Step::Scale(f / design));
        steps
    }

    /// Where a design-unit point lands on the framebuffer.
    #[cfg(test)]
    pub fn map(&self, design: f32, (px, py): (f32, f32)) -> (f32, f32) {
        let (mut x, mut y) = (px, py);
        for step in self.transform(design).iter().rev() {
            (x, y) = match *step {
                Step::Scale(s) => (x * s, y * s),
                Step::Rotate(a) => (x * a.cos() - y * a.sin(), x * a.sin() + y * a.cos()),
                Step::Translate(dx, dy) => (x + dx, y + dy),
            };
        }
        (x, y)
    }
}

/// One canvas transform, applied in order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    Translate(f32, f32),
    Rotate(f32),
    Scale(f32),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01
    }

    #[test]
    fn the_gen3_theme_is_turned_onto_its_sideways_panel() {
        let g = Geometry::for_framebuffer(480, 640);
        // The upright theme's top lies along the framebuffer's right edge.
        assert!(close(g.map(480.0, (240.0, 0.0)), (480.0, 240.0)));
        assert!(close(g.map(480.0, (0.0, 0.0)), (480.0, 0.0)));
        assert!(close(g.map(480.0, (240.0, 240.0)), (240.0, 240.0)));
    }

    #[test]
    fn themes_scale_to_smaller_themes() {
        let g = Geometry::for_framebuffer(320, 320);
        assert_eq!(g.theme, 320.0);
        assert!(close(g.map(480.0, (480.0, 480.0)), (320.0, 320.0)));
        assert!(close(g.map(480.0, (240.0, 0.0)), (160.0, 0.0)));
    }

    #[test]
    fn an_unknown_panel_centres_its_largest_circle() {
        let g = Geometry::for_framebuffer(800, 480);
        assert_eq!((g.theme, g.origin, g.quarter_turns), (480.0, (160.0, 0.0), 0));
    }
}
