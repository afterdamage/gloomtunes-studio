//! Flush-to-zero for the audio thread (ARCHITECTURE.md §7.6).
//!
//! When a filter, delay or reverb decays towards silence its state passes through subnormal
//! floats, which x86 CPUs handle in microcode up to a hundred times slower. With FTZ (results
//! flush to zero) and DAZ (inputs read as zero) set in MXCSR, those values become 0 instead.
//! On 64-bit ARM (Apple Silicon, D91) the FZ bit of FPCR does both. ARM cores pay less for
//! subnormals, but setting it keeps the sound identical to x86 and the cost flat.

/// Sets flush-to-zero for as long as it lives and restores the previous mode when dropped, so
/// the callback (or export thread) gets it and nothing else on that thread is changed.
/// Does nothing on other architectures.
pub struct DenormalGuard {
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    previous: fpmode::Word,
}

#[cfg(target_arch = "x86_64")]
mod fpmode {
    pub type Word = u32;
    /// Flush to zero (bit 15) and denormals are zero (bit 6) in MXCSR.
    pub const FLUSH: Word = (1 << 15) | (1 << 6);

    #[allow(unsafe_code)]
    pub fn get() -> Word {
        let mut v: u32 = 0;
        // SAFETY: `stmxcsr` stores the 32-bit MXCSR register to the given address, which is
        // a valid, aligned, writable u32 on our stack. SSE is part of the x86_64 baseline.
        unsafe {
            std::arch::asm!("stmxcsr [{}]", in(reg) &mut v, options(nostack, preserves_flags));
        }
        v
    }

    #[allow(unsafe_code)]
    pub fn set(v: Word) {
        // SAFETY: `ldmxcsr` loads MXCSR from a valid aligned u32. Only values read from MXCSR
        // with the FTZ and DAZ bits added are passed here; both bits are supported by every
        // x86_64 CPU, so no reserved bit is set and the instruction cannot fault.
        unsafe {
            std::arch::asm!("ldmxcsr [{}]", in(reg) &v, options(nostack, readonly, preserves_flags));
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod fpmode {
    pub type Word = u64;
    /// FZ (bit 24) in FPCR: subnormal inputs and results become zero.
    pub const FLUSH: Word = 1 << 24;

    #[allow(unsafe_code)]
    pub fn get() -> Word {
        let v: u64;
        // SAFETY: reading FPCR is allowed at EL0 on every AArch64 CPU and has no side effects.
        unsafe {
            std::arch::asm!("mrs {}, fpcr", out(reg) v, options(nomem, nostack, preserves_flags));
        }
        v
    }

    #[allow(unsafe_code)]
    pub fn set(v: Word) {
        // SAFETY: writing FPCR is allowed at EL0. Only values read from FPCR with FZ added are
        // passed here, and FZ is part of the base AArch64 floating-point architecture.
        unsafe {
            std::arch::asm!("msr fpcr, {}", in(reg) v, options(nostack, preserves_flags));
        }
    }
}

impl DenormalGuard {
    /// Turns flush-to-zero on for the current thread.
    #[inline]
    pub fn new() -> Self {
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        {
            let previous = fpmode::get();
            if previous & fpmode::FLUSH != fpmode::FLUSH {
                fpmode::set(previous | fpmode::FLUSH);
            }
            Self { previous }
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
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
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        if self.previous & fpmode::FLUSH != fpmode::FLUSH {
            fpmode::set(self.previous);
        }
    }
}

#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
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
