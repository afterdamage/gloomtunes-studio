//! What a note carries besides its key and velocity.

/// How a note joins the one before it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Glide {
    /// A note of its own.
    #[default]
    None,
    /// Starts at the previous note's pitch and glides to its own (portamento).
    Porta,
    /// Starts no voice: the most recent sounding note glides to this key over `seconds`.
    Slide {
        /// Glide time: the slide note's length.
        seconds: f32,
    },
}

/// Per-note values: pan, fine pitch, release velocity, two generic modulation values and the
/// glide mode. [`NoteExpr::default`] changes nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoteExpr {
    /// Stereo position, -1 to 1.
    pub pan: f32,
    /// Fine pitch in cents.
    pub fine: f32,
    /// Release velocity, 0 to 1 (0.5 neutral).
    pub release: f32,
    /// Mod X, 0 to 1 (0.5 neutral).
    pub mod_x: f32,
    /// Mod Y, 0 to 1 (0.5 neutral).
    pub mod_y: f32,
    /// Slide or portamento.
    pub glide: Glide,
}

impl Default for NoteExpr {
    fn default() -> Self {
        Self {
            pan: 0.0,
            fine: 0.0,
            release: 0.5,
            mod_x: 0.5,
            mod_y: 0.5,
            glide: Glide::None,
        }
    }
}

impl NoteExpr {
    /// Release time multiplier: twice as long at release velocity 0, half as long at 1.
    pub fn release_scale(&self) -> f32 {
        ((0.5 - self.release.clamp(0.0, 1.0)) * 2.0).exp2()
    }

    /// Mod X as a bipolar value, -1 to 1 (0 neutral).
    pub fn x(&self) -> f32 {
        (self.mod_x.clamp(0.0, 1.0) - 0.5) * 2.0
    }

    /// Mod Y as a bipolar value, -1 to 1 (0 neutral).
    pub fn y(&self) -> f32 {
        (self.mod_y.clamp(0.0, 1.0) - 0.5) * 2.0
    }
}
