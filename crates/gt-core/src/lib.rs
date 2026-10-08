//! Shared plain-data types for GloomTunes Studio: musical time, IDs and the project document.
//!
//! Step 1 only defines the time base. The document types described in ARCHITECTURE.md §6 arrive
//! with the steps that need them.

#![forbid(unsafe_code)]

/// Ticks per quarter note. 960 divides evenly by 2, 3, 4, 5, 6, 8, 10, 12, 15, 16, 20, 24, 32 and
/// 64, so straight notes down to 1/256 and triplets down to 1/128 are exact integers.
pub const PPQ: i64 = 960;

#[cfg(test)]
mod tests {
    use super::PPQ;

    #[test]
    fn common_divisions_are_exact() {
        // 1/16 note, 1/16 triplet, 1/32 triplet, 1/256 note.
        for div in [4, 6, 12, 64] {
            assert_eq!(PPQ % div, 0, "1/{div} of a quarter");
        }
    }
}
