//! Effect system representation and bitset operations.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EffectFlags(pub u32);

impl EffectFlags {
    pub const NONE: Self = Self(0);

    // Authority / Security
    pub const NET: Self = Self(1 << 0);
    pub const FILE_READ: Self = Self(1 << 1);
    pub const FILE_WRITE: Self = Self(1 << 2);
    pub const ENVIRONMENT: Self = Self(1 << 3);
    pub const PROCESS: Self = Self(1 << 4);
    pub const FOREIGN: Self = Self(1 << 5);

    // Resources
    pub const HEAP: Self = Self(1 << 6);
    pub const BLOCKING: Self = Self(1 << 7);
    pub const SUSPEND: Self = Self(1 << 8);
    pub const THREAD: Self = Self(1 << 9);

    // Semantics
    pub const PURE: Self = Self(1 << 10);
    pub const READONLY: Self = Self(1 << 11);
    pub const NO_ALLOC: Self = Self(1 << 12);
    pub const NO_THROW: Self = Self(1 << 13);
    pub const NO_SUSPEND: Self = Self(1 << 14);

    /// Every authority and resource effect: what a call through a function
    /// value may perform, since its target is not known statically.
    pub const ANY_CALL: Self = Self((1 << 10) - 1);

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Net" => Some(Self::NET),
            "FileRead" => Some(Self::FILE_READ),
            "FileWrite" => Some(Self::FILE_WRITE),
            "Environment" => Some(Self::ENVIRONMENT),
            "Process" => Some(Self::PROCESS),
            "Foreign" => Some(Self::FOREIGN),
            "Heap" => Some(Self::HEAP),
            "Blocking" => Some(Self::BLOCKING),
            "Suspend" => Some(Self::SUSPEND),
            "Thread" => Some(Self::THREAD),
            "Pure" => Some(Self::PURE),
            "Readonly" => Some(Self::READONLY),
            "NoAlloc" => Some(Self::NO_ALLOC),
            "NoThrow" => Some(Self::NO_THROW),
            "NoSuspend" => Some(Self::NO_SUSPEND),
            _ => None,
        }
    }

    pub fn to_names(self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.contains(Self::NET) {
            names.push("Net");
        }
        if self.contains(Self::FILE_READ) {
            names.push("FileRead");
        }
        if self.contains(Self::FILE_WRITE) {
            names.push("FileWrite");
        }
        if self.contains(Self::ENVIRONMENT) {
            names.push("Environment");
        }
        if self.contains(Self::PROCESS) {
            names.push("Process");
        }
        if self.contains(Self::FOREIGN) {
            names.push("Foreign");
        }
        if self.contains(Self::HEAP) {
            names.push("Heap");
        }
        if self.contains(Self::BLOCKING) {
            names.push("Blocking");
        }
        if self.contains(Self::SUSPEND) {
            names.push("Suspend");
        }
        if self.contains(Self::THREAD) {
            names.push("Thread");
        }
        if self.contains(Self::PURE) {
            names.push("Pure");
        }
        if self.contains(Self::READONLY) {
            names.push("Readonly");
        }
        if self.contains(Self::NO_ALLOC) {
            names.push("NoAlloc");
        }
        if self.contains(Self::NO_THROW) {
            names.push("NoThrow");
        }
        if self.contains(Self::NO_SUSPEND) {
            names.push("NoSuspend");
        }
        names
    }
}
