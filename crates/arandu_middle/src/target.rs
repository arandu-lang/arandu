//! Canonical platform knowledge for linking, C drivers, and system libraries.
//!
//! Data layout remains in [`crate::layout`]. This module is the single source
//! of truth for *which* libraries a host toolchain needs. Tests, the C
//! backend, LLVM execution, and the CLI linker ask a [`TargetSpec`] instead of
//! hardcoding `-lm`, `-lpthread`, or `bcrypt.lib`.

/// Operating system family used for linker and PAL selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TargetOs {
    Linux,
    Macos,
    Windows,
    Unknown,
}

/// ABI/environment that selects GNU versus MSVC link flavor on Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TargetEnv {
    Gnu,
    Msvc,
    Darwin,
    Unknown,
}

/// Command-line convention of the linker or compiler driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkerFlavor {
    /// `cc`/`clang`/`gcc`: `-lfoo`, `-Wl,...`.
    GnuCc,
    /// `link.exe` / `lld-link`: `foo.lib`, `/OUT:`.
    MsvcLink,
    /// Apple `cc`/`clang` driver.
    DarwinCc,
}

/// Immutable platform triple used by backends, tests, and the CLI linker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TargetSpec {
    os: TargetOs,
    env: TargetEnv,
}

/// Linker argv fragments derived from a [`TargetSpec`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LinkerPlan {
    spec: TargetSpec,
    flavor: LinkerFlavor,
}

/// Canonical freestanding profile. Exact triples only.
///
/// The compiler does not invent Flash or RAM maps, vector tables, or a linker
/// script. A final ELF exists only when the user supplies `--linker-script`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EmbeddedProfile {
    triple: &'static str,
    /// Tripla canônica que o LLVM/clang imprime em `-print-target-triple`.
    ///
    /// A forma curta (`thumbv6m-none-eabi`) é o nome público do alvo; clang a
    /// normaliza para `thumbv6m-unknown-none-eabi` e avisa
    /// `-Woverride-module` quando o módulo declara a forma curta.
    llvm_triple: &'static str,
    /// Pointer size in bytes. Never read from the host.
    pointer_bytes: u8,
    /// Pointer alignment in bytes.
    pointer_align: u8,
    /// LLVM module `target datalayout` string for this triple.
    llvm_datalayout: &'static str,
}

const THUMBV6M_NONE_EABI: EmbeddedProfile = EmbeddedProfile {
    triple: "thumbv6m-none-eabi",
    llvm_triple: "thumbv6m-unknown-none-eabi",
    pointer_bytes: 4,
    pointer_align: 4,
    // Same 32-bit ARM layout LLVM reports for `thumbv6m-none-eabi`
    // (`clang -target thumbv6m-none-eabi -S -emit-llvm`).
    llvm_datalayout: "e-m:e-p:32:32-Fi8-i64:64-v128:64:128-a:0:32-n32-S64",
};

const THUMBV7EM_NONE_EABIHF: EmbeddedProfile = EmbeddedProfile {
    triple: "thumbv7em-none-eabihf",
    llvm_triple: "thumbv7em-unknown-none-eabihf",
    pointer_bytes: 4,
    pointer_align: 4,
    llvm_datalayout: "e-m:e-p:32:32-Fi8-i64:64-v128:64:128-a:0:32-n32-S64",
};

const RISCV32IMC_UNKNOWN_NONE_ELF: EmbeddedProfile = EmbeddedProfile {
    triple: "riscv32imc-unknown-none-elf",
    llvm_triple: "riscv32imc-unknown-none-elf",
    pointer_bytes: 4,
    pointer_align: 4,
    llvm_datalayout: "e-m:e-p:32:32-i64:64-n32-S128",
};

impl EmbeddedProfile {
    #[must_use]
    pub const fn triple(self) -> &'static str {
        self.triple
    }

    /// Tripla canônica usada no cabeçalho do módulo LLVM e em `clang --target`.
    #[must_use]
    pub const fn llvm_triple(self) -> &'static str {
        self.llvm_triple
    }

    #[must_use]
    pub const fn pointer_bytes(self) -> u8 {
        self.pointer_bytes
    }

    #[must_use]
    pub const fn pointer_align(self) -> u8 {
        self.pointer_align
    }

    #[must_use]
    pub const fn llvm_datalayout(self) -> &'static str {
        self.llvm_datalayout
    }
}

/// Look up a canonical bare-metal triple. Unknown triples are not guessed.
#[must_use]
pub fn embedded_profile(triple: &str) -> Option<&'static EmbeddedProfile> {
    match triple {
        "thumbv6m-none-eabi" => Some(&THUMBV6M_NONE_EABI),
        "thumbv7em-none-eabihf" => Some(&THUMBV7EM_NONE_EABIHF),
        "riscv32imc-unknown-none-elf" => Some(&RISCV32IMC_UNKNOWN_NONE_ELF),
        _ => None,
    }
}

impl TargetSpec {
    #[must_use]
    pub const fn linux_gnu() -> Self {
        Self {
            os: TargetOs::Linux,
            env: TargetEnv::Gnu,
        }
    }

    #[must_use]
    pub const fn macos() -> Self {
        Self {
            os: TargetOs::Macos,
            env: TargetEnv::Darwin,
        }
    }

    #[must_use]
    pub const fn windows_msvc() -> Self {
        Self {
            os: TargetOs::Windows,
            env: TargetEnv::Msvc,
        }
    }

    #[must_use]
    pub const fn windows_gnu() -> Self {
        Self {
            os: TargetOs::Windows,
            env: TargetEnv::Gnu,
        }
    }

    /// Compile-time host. Prefer this over `cfg!(windows)` at call sites.
    #[must_use]
    pub const fn for_host() -> Self {
        #[cfg(all(windows, target_env = "msvc"))]
        {
            Self::windows_msvc()
        }
        #[cfg(all(windows, not(target_env = "msvc")))]
        {
            Self::windows_gnu()
        }
        #[cfg(target_os = "macos")]
        {
            Self::macos()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            Self::linux_gnu()
        }
        #[cfg(not(any(windows, unix)))]
        {
            Self {
                os: TargetOs::Unknown,
                env: TargetEnv::Unknown,
            }
        }
    }

    #[must_use]
    pub const fn os(self) -> TargetOs {
        self.os
    }

    #[must_use]
    pub const fn env(self) -> TargetEnv {
        self.env
    }

    #[must_use]
    pub const fn is_windows(self) -> bool {
        matches!(self.os, TargetOs::Windows)
    }

    #[must_use]
    pub const fn linker_flavor(self) -> LinkerFlavor {
        match self.os {
            TargetOs::Windows => match self.env {
                TargetEnv::Msvc => LinkerFlavor::MsvcLink,
                _ => LinkerFlavor::GnuCc,
            },
            TargetOs::Macos => LinkerFlavor::DarwinCc,
            TargetOs::Linux | TargetOs::Unknown => LinkerFlavor::GnuCc,
        }
    }

    #[must_use]
    pub const fn linker_plan(self) -> LinkerPlan {
        LinkerPlan {
            spec: self,
            flavor: self.linker_flavor(),
        }
    }

    /// Math library flags for a GNU-style C driver. Empty on Windows CRT.
    #[must_use]
    pub const fn math_libraries(self) -> &'static [&'static str] {
        match self.os {
            TargetOs::Windows => &[],
            TargetOs::Linux | TargetOs::Macos | TargetOs::Unknown => &["-lm"],
        }
    }

    /// System libraries for generated C / LLVM IR linked with `clang`/`gcc`.
    #[must_use]
    pub const fn runtime_system_libs(self) -> &'static [&'static str] {
        match self.os {
            TargetOs::Windows => &[
                "-lkernel32",
                "-lntdll",
                "-luserenv",
                "-lws2_32",
                "-lbcrypt",
                "-ldbghelp",
            ],
            TargetOs::Macos => &["-lpthread"],
            TargetOs::Linux => &["-lpthread", "-ldl"],
            TargetOs::Unknown => &["-lpthread"],
        }
    }

    /// Combined GNU-style driver args: system libraries then math.
    #[must_use]
    pub const fn cc_link_args(self) -> &'static [&'static str] {
        match self.os {
            TargetOs::Windows => self.runtime_system_libs(),
            TargetOs::Macos => &["-lpthread", "-lm"],
            TargetOs::Linux => &["-lpthread", "-ldl", "-lm"],
            TargetOs::Unknown => &["-lpthread", "-lm"],
        }
    }

    /// MSVC `link.exe` / `lld-link` libraries for the packaged native runtime.
    #[must_use]
    pub const fn msvc_runtime_libs(self) -> &'static [&'static str] {
        if !self.is_windows() {
            return &[];
        }
        &[
            "kernel32.lib",
            "ntdll.lib",
            "userenv.lib",
            "ws2_32.lib",
            "bcrypt.lib",
            "dbghelp.lib",
            "msvcrt.lib",
        ]
    }

    /// Libraries the checkout-only `rustc` link fallback must name explicitly.
    ///
    /// `kernel32` is already a default MSVC library. `msvcrt` is omitted so the
    /// fallback does not introduce a second CRT next to the one rustc selects.
    #[must_use]
    pub const fn msvc_rustc_link_args(self) -> &'static [&'static str] {
        match self.linker_flavor() {
            LinkerFlavor::MsvcLink => &[
                "-C",
                "link-arg=ntdll.lib",
                "-C",
                "link-arg=userenv.lib",
                "-C",
                "link-arg=ws2_32.lib",
                "-C",
                "link-arg=bcrypt.lib",
                "-C",
                "link-arg=dbghelp.lib",
            ],
            LinkerFlavor::GnuCc | LinkerFlavor::DarwinCc => &[],
        }
    }

    /// Extra GNU-style args when linking Cranelift objects with the packaged runtime.
    #[must_use]
    pub const fn native_aot_cc_libs(self) -> &'static [&'static str] {
        match self.os {
            TargetOs::Linux => &[
                "-lgcc_s",
                "-lutil",
                "-lrt",
                "-lpthread",
                "-lm",
                "-ldl",
                "-lc",
            ],
            TargetOs::Macos => &["-liconv", "-lSystem"],
            TargetOs::Windows => self.runtime_system_libs(),
            TargetOs::Unknown => &["-lpthread", "-lm", "-lc"],
        }
    }

    #[must_use]
    pub const fn darwin_frameworks(self) -> &'static [&'static str] {
        match self.os {
            TargetOs::Macos => &["Security", "CoreFoundation"],
            _ => &[],
        }
    }
}

impl LinkerPlan {
    #[must_use]
    pub const fn spec(self) -> TargetSpec {
        self.spec
    }

    #[must_use]
    pub const fn flavor(self) -> LinkerFlavor {
        self.flavor
    }

    /// Names preferred before the system `cc`/`link.exe` driver.
    #[must_use]
    pub const fn preferred_lld_names(self) -> &'static [&'static str] {
        match self.flavor {
            LinkerFlavor::MsvcLink => &["lld-link", "rust-lld"],
            LinkerFlavor::DarwinCc => &["ld64.lld", "ld.lld", "lld"],
            LinkerFlavor::GnuCc => &["ld.lld", "lld", "ld64.lld"],
        }
    }

    #[must_use]
    pub const fn cc_fuse_lld_flag(self) -> &'static str {
        match self.flavor {
            LinkerFlavor::MsvcLink => "-fuse-ld=lld-link",
            _ => "-fuse-ld=lld",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_spec_omits_libm_on_windows_and_libdl_on_macos() {
        let spec = TargetSpec::for_host();
        let args = spec.cc_link_args();
        #[cfg(windows)]
        {
            assert!(!args.contains(&"-lm"));
            assert!(args.contains(&"-lbcrypt"));
            assert!(args.contains(&"-lws2_32"));
            assert!(args.contains(&"-lkernel32"));
            assert!(spec.msvc_runtime_libs().contains(&"bcrypt.lib"));
        }
        #[cfg(target_os = "macos")]
        {
            assert!(args.contains(&"-lm"));
            assert!(args.contains(&"-lpthread"));
            assert!(!args.contains(&"-ldl"));
            assert!(spec.darwin_frameworks().contains(&"Security"));
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            assert!(args.contains(&"-lm"));
            assert!(args.contains(&"-lpthread"));
            assert!(args.contains(&"-ldl"));
        }
    }

    #[test]
    fn windows_gnu_and_msvc_share_cc_system_libs_but_not_libm() {
        for spec in [TargetSpec::windows_msvc(), TargetSpec::windows_gnu()] {
            assert!(spec.math_libraries().is_empty());
            assert_eq!(spec.cc_link_args(), spec.runtime_system_libs());
        }
        assert_eq!(
            TargetSpec::windows_msvc().linker_flavor(),
            LinkerFlavor::MsvcLink
        );
        assert_eq!(
            TargetSpec::windows_gnu().linker_flavor(),
            LinkerFlavor::GnuCc
        );
    }

    #[test]
    fn linux_tables_include_math_pthread_and_dl() {
        let spec = TargetSpec::linux_gnu();
        assert_eq!(spec.math_libraries(), &["-lm"]);
        assert!(spec.cc_link_args().contains(&"-ldl"));
        assert!(spec.native_aot_cc_libs().contains(&"-lrt"));
        assert!(spec.msvc_runtime_libs().is_empty());
        assert!(spec.msvc_rustc_link_args().is_empty());
    }

    #[test]
    fn embedded_profiles_are_exact_32_bit_triples_without_a_memory_map() {
        let arm = embedded_profile("thumbv7em-none-eabihf").expect("arm profile");
        assert_eq!(arm.pointer_bytes(), 4);
        assert_eq!(arm.pointer_align(), 4);
        assert_eq!(
            arm.llvm_datalayout(),
            "e-m:e-p:32:32-Fi8-i64:64-v128:64:128-a:0:32-n32-S64"
        );
        let riscv = embedded_profile("riscv32imc-unknown-none-elf").expect("riscv profile");
        assert_eq!(riscv.pointer_bytes(), 4);
        assert_eq!(riscv.pointer_align(), 4);
        assert_eq!(riscv.llvm_datalayout(), "e-m:e-p:32:32-i64:64-n32-S128");
        let cortex_m0 = embedded_profile("thumbv6m-none-eabi").expect("cortex-m0 profile");
        assert_eq!(cortex_m0.pointer_bytes(), 4);
        assert_eq!(cortex_m0.pointer_align(), 4);
        // LLVM reports the same ARM layout for every 32-bit Thumb profile.
        assert_eq!(cortex_m0.llvm_datalayout(), arm.llvm_datalayout());
        assert_ne!(arm.llvm_datalayout(), riscv.llvm_datalayout());
        assert!(embedded_profile("thumbv7em-none-eabi").is_none());
        assert!(embedded_profile("riscv32-unknown-none-elf").is_none());
        assert!(embedded_profile("x86_64-unknown-linux-gnu").is_none());
    }

    #[test]
    fn msvc_rustc_fallback_names_non_default_system_libs() {
        let args = TargetSpec::windows_msvc().msvc_rustc_link_args();
        assert!(args.contains(&"link-arg=bcrypt.lib"));
        assert!(args.contains(&"link-arg=ws2_32.lib"));
        assert!(args.contains(&"link-arg=userenv.lib"));
        assert!(args.iter().all(|arg| !arg.contains("msvcrt")));
        assert!(TargetSpec::windows_gnu().msvc_rustc_link_args().is_empty());
    }
}
