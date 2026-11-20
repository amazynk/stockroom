//! Metered connections: background updates skip them.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metered {
    Yes,
    No,
    Unknown,
}

pub trait NetworkProbe: fmt::Debug + Send + Sync {
    fn metered(&self) -> Metered;
}

/// A fixed answer (tests, `--root`).
#[derive(Debug, Clone, Copy)]
pub struct FixedNetwork(pub Metered);

impl NetworkProbe for FixedNetwork {
    fn metered(&self) -> Metered {
        self.0
    }
}

/// The real answer: `NetworkManager`'s `Metered` property over D-Bus on Linux; "unknown"
/// elsewhere for now (Windows `GetNetworkConnectivityHint` comes with milestone 4).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemNetwork;

impl NetworkProbe for SystemNetwork {
    fn metered(&self) -> Metered {
        #[cfg(target_os = "linux")]
        {
            match network_manager_metered() {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!(error = %e, "NetworkManager not reachable; metered state unknown");
                    Metered::Unknown
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Metered::Unknown
        }
    }
}

/// Maps `NetworkManager`'s `NMMetered` enum.
pub fn from_nm_metered(v: u32) -> Metered {
    match v {
        1 | 3 => Metered::Yes,
        2 | 4 => Metered::No,
        _ => Metered::Unknown,
    }
}

#[cfg(target_os = "linux")]
fn network_manager_metered() -> zbus::Result<Metered> {
    let conn = zbus::blocking::Connection::system()?;
    let proxy = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
    )?;
    let v: u32 = proxy.get_property("Metered")?;
    Ok(from_nm_metered(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nm_values() {
        assert_eq!(from_nm_metered(1), Metered::Yes);
        assert_eq!(from_nm_metered(3), Metered::Yes);
        assert_eq!(from_nm_metered(4), Metered::No);
        assert_eq!(from_nm_metered(0), Metered::Unknown);
        assert_eq!(FixedNetwork(Metered::Yes).metered(), Metered::Yes);
    }
}
