//! Facts about the computer, as far as the catalog cares. `suite-platform` detects them;
//! here they are plain data so matching is testable on any machine.

use std::{cmp::Ordering, fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::error::CatalogError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Linux,
    Macos,
    Windows,
}

impl Os {
    /// The OS this binary was built for.
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Windows => "windows",
        }
    }

    /// The name people know it by, for messages (`as_str` is the catalog's spelling).
    pub fn name(self) -> &'static str {
        match self {
            Self::Linux => "Linux",
            Self::Macos => "macOS",
            Self::Windows => "Windows",
        }
    }
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    #[serde(rename = "x86_64")]
    X86_64,
    Aarch64,
    /// macOS universal binaries (x86_64 + arm64).
    Universal,
}

impl Arch {
    /// The architecture this binary was built for (`Universal` is never "current").
    pub const fn current() -> Self {
        if cfg!(target_arch = "aarch64") { Self::Aarch64 } else { Self::X86_64 }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
            Self::Universal => "universal",
        }
    }

    /// The name people know it by, for messages.
    pub fn name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86-64",
            Self::Aarch64 => "ARM",
            Self::Universal => "universal",
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A dotted numeric version such as glibc `2.35` or macOS `14.6.1`; missing parts are 0.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DottedVersion(Vec<u32>);

impl DottedVersion {
    pub fn new(parts: impl Into<Vec<u32>>) -> Self {
        Self(parts.into())
    }

    pub fn parts(&self) -> &[u32] {
        &self.0
    }
}

impl FromStr for DottedVersion {
    type Err = CatalogError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Result<Vec<u32>, _> = s.trim().split('.').map(str::parse).collect();
        match parts {
            Ok(p) if !p.is_empty() && p.len() <= 4 => Ok(Self(p)),
            _ => Err(CatalogError::Invalid(format!("bad version {s:?}"))),
        }
    }
}

impl TryFrom<String> for DottedVersion {
    type Error = CatalogError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<DottedVersion> for String {
    fn from(v: DottedVersion) -> Self {
        v.to_string()
    }
}

impl fmt::Display for DottedVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, p) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            write!(f, "{p}")?;
        }
        Ok(())
    }
}

impl PartialEq for DottedVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for DottedVersion {}

impl PartialOrd for DottedVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DottedVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        let n = self.0.len().max(other.0.len());
        (0..n)
            .map(|i| self.0.get(i).copied().unwrap_or(0).cmp(&other.0.get(i).copied().unwrap_or(0)))
            .find(|o| o.is_ne())
            .unwrap_or(Ordering::Equal)
    }
}

/// What the computer is and can run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostFacts {
    pub os: Os,
    pub arch: Arch,
    /// CPU supports the x86-64-v3 level (AVX2, BMI1/2, FMA, F16C, LZCNT, MOVBE).
    pub x86_64_v3: bool,
    /// glibc version; `None` on musl and non-Linux systems.
    pub glibc: Option<DottedVersion>,
    pub macos: Option<DottedVersion>,
    pub windows_build: Option<u32>,
    /// `flatpak` is installed (Linux).
    pub flatpak: bool,
}

impl HostFacts {
    /// A typical modern machine of the given OS; for tests and examples.
    pub fn example(os: Os) -> Self {
        Self {
            os,
            arch: Arch::X86_64,
            x86_64_v3: true,
            glibc: (os == Os::Linux).then(|| DottedVersion::new([2, 39])),
            macos: (os == Os::Macos).then(|| DottedVersion::new([14, 6])),
            windows_build: (os == Os::Windows).then_some(22631),
            flatpak: os == Os::Linux,
        }
    }
}
