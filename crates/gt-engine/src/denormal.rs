//! Flush-to-zero for the audio thread (ARCHITECTURE.md §7.6).
//!
//! When a filter, delay or reverb decays towards silence its state passes through subnormal
//! floats, which x86 CPUs handle in microcode up to a hundred times slower. With FTZ (results
//! flush to zero) and DAZ (inputs read as zero) set in MXCSR, those values become 0 instead.

/// Sets FTZ and DAZ for as long as it lives and restores the previous mode when dropped, so
/// the callback (or export thread) gets them and nothing else on that thread is changed.
/// Does nothing on other architectures.
pub struct DenormalGuard {
    #[cfg(target_arch = "x86_64")]
    previous: u32,
}

#[cfg(target_arch = "x86_64")]
mod mxcsr {
    /// Flush to zero (bit 15) and denormals are zero (bit 6).
    pub const FTZ_DAZ: u32 = (1 << 15) | (1 << 6);

    #[allow(unsafe_code)]
    pub fn get() -> u32 {
        let mut v: u32 = 0;
        // SAFETY: `stmxcsr` stores the 32-bit MXCSR register to the given address, which is
        // a valid, aligned, writable u32 on our stack. SSE is part of the x86_64 baseline.
        unsafe {
            std::arch::asm!("stmxcsr [{}]", in(reg) &mut v, options(nostack, preserves_flags));
        }
        v
    }

    #[allow(unsafe_code)]
    pub fn set(v: u32) {
        // SAFETY: `ldmxcsr` loads MXCSR from a valid aligned u32. Only values read from MXCSR
        // with the FTZ and DAZ bits added are passed here; both bits are supported by every
        // x86_64 CPU, so no reserved bit is set and the instruction cannot fault.
        unsafe {
            std::arch::asm!("ldmxcsr [{}]", in(reg) &v, options(nostack, readonly, preserves_flags));
        }
    }
}

impl DenormalGuard {
    /// Turns flush-to-zero on for the current thread.
    #[inline]
    pub fn new() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            let previous = mxcsr::get();
            if previous & mxcsr::FTZ_DAZ != mxcsr::FTZ_DAZ {
                mxcsr::set(previous | mxcsr::FTZ_DAZ);
            }
            Self { previous }
        }
        #[cfg(not(target_arch = "x86_64"))]
        Self {}
    }
}

impl Default for DenormalGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DenormalGuard {
    #[inline]
    fn drop(&mut self) {
        #[cfg(target_arch = "x86_64")]
        if self.previous & mxcsr::FTZ_DAZ != mxcsr::FTZ_DAZ {
            mxcsr::set(self.previous);
        }
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;
    use std::hint::black_box;

    #[test]
    fn subnormals_flush_to_zero_inside_the_guard_only() {
        let tiny = black_box(f32::MIN_POSITIVE);
        assert!((black_box(tiny) * 0.5).is_subnormal());
        {
            let _g = DenormalGuard::new();
            assert_eq!(black_box(tiny) * 0.5, 0.0);
            // DAZ: a subnormal input reads as zero.
            let sub = f32::from_bits(1);
            assert_eq!(black_box(sub) * 1.0, 0.0);
        }
        assert!((black_box(tiny) * 0.5).is_subnormal());
    }

    #[test]
    fn nested_guards_keep_the_outer_mode() {
        let tiny = black_box(f32::MIN_POSITIVE);
        let _outer = DenormalGuard::new();
        drop(DenormalGuard::new());
        assert_eq!(black_box(tiny) * 0.5, 0.0);
    }
}
