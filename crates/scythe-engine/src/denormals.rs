//! Flush-to-zero and denormals-are-zero around audio processing. Hosts differ in whether their
//! audio thread sets them, and a plugin should neither output subnormals nor slow down on them in
//! any host. AArch64 starts with them off, so without this its output can hold subnormals that an
//! x86 host's flags would have hidden.

/// Sets the flags while it lives and puts the caller's floating-point state back when dropped.
#[must_use]
pub(crate) struct FlushDenormals {
    /// The caller's state, when it had the flags off and this turned them on.
    saved: Option<arch::State>,
}

impl FlushDenormals {
    pub(crate) fn new() -> Self {
        let state = arch::get();
        if state & arch::FLUSH == arch::FLUSH {
            return Self { saved: None };
        }
        // SAFETY: Rust and LLVM assume the default floating-point environment, so changing FTZ/DAZ
        // around Rust code is outside what they promise; it is accepted audio practice all the same.
        // Only subnormal results change, the guarded region reaches its data through memory, and
        // the asm's memory clobber keeps those loads and stores between the two writes.
        arch::set(state | arch::FLUSH);
        Self { saved: Some(state) }
    }
}

impl Drop for FlushDenormals {
    fn drop(&mut self) {
        if let Some(state) = self.saved {
            arch::set(state);
        }
    }
}

/// `sse` is an x86 and x86_64 feature only: MXCSR comes with it.
#[cfg(target_feature = "sse")]
mod arch {
    use core::arch::asm;

    pub type State = u32;
    /// MXCSR's FTZ (bit 15) and DAZ (bit 6).
    pub const FLUSH: State = 1 << 15 | 1 << 6;

    pub fn get() -> State {
        let mut csr: State = 0;
        unsafe { asm!("stmxcsr dword ptr [{}]", in(reg) &mut csr, options(nostack, preserves_flags)) };
        csr
    }

    /// Also puts back the exception flags as they were, so it does not preserve the flags.
    pub fn set(csr: State) {
        unsafe { asm!("ldmxcsr dword ptr [{}]", in(reg) &csr, options(nostack)) };
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    use core::arch::asm;

    pub type State = u64;
    /// FPCR.FZ, which flushes subnormal inputs and results alike.
    pub const FLUSH: State = 1 << 24;

    pub fn get() -> State {
        let fpcr: State;
        unsafe { asm!("mrs {}, fpcr", out(reg) fpcr, options(nostack, preserves_flags)) };
        fpcr
    }

    pub fn set(fpcr: State) {
        unsafe { asm!("msr fpcr, {}", in(reg) fpcr, options(nostack, preserves_flags)) };
    }
}

/// Elsewhere there is nothing to set: with no flush bits, `FlushDenormals::new` writes nothing.
#[cfg(not(any(target_feature = "sse", target_arch = "aarch64")))]
mod arch {
    pub type State = u8;
    pub const FLUSH: State = 0;

    pub fn get() -> State {
        0
    }

    pub fn set(_: State) {}
}

#[cfg(all(test, any(target_feature = "sse", target_arch = "aarch64")))]
mod tests {
    use super::*;
    use std::hint::black_box;

    fn product() -> f32 {
        black_box(1e-36f32) * black_box(1e-3f32)
    }

    #[test]
    fn subnormal_results_are_flushed_while_it_lives_and_not_after() {
        assert!(product().is_subnormal());
        {
            let _flushed = FlushDenormals::new();
            assert_eq!(product(), 0.0);
            assert_eq!(black_box(f32::from_bits(1)) * black_box(1.0f32), 0.0, "a subnormal input reads as zero");
        }
        assert!(product().is_subnormal());
    }

    #[test]
    fn a_caller_that_already_flushes_keeps_flushing() {
        let outer = FlushDenormals::new();
        {
            let _inner = FlushDenormals::new();
            assert_eq!(product(), 0.0);
        }
        assert_eq!(product(), 0.0, "the inner guard left the outer one's flags alone");
        drop(outer);
        assert!(product().is_subnormal());
    }
}
