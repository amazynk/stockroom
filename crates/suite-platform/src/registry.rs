//! The Windows registry behind a trait. The real one (HKCU/HKLM via `winreg`) only exists on
//! Windows; the in-memory one, optionally saved to a JSON file, is used by tests and `--root`
//! on every OS, so the Windows integration code is exercised on Linux too.

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub enum Hive {
    /// `HKEY_CURRENT_USER`: the only hive Stockroom writes.
    CurrentUser,
    /// `HKEY_LOCAL_MACHINE`: read only, for detection.
    LocalMachine,
}

/// A registry value; only the types Stockroom uses.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Value {
    String(String),
    Dword(u32),
}

pub trait Registry: fmt::Debug + Send + Sync {
    fn get(&self, hive: Hive, key: &str, name: &str) -> std::io::Result<Option<Value>>;
    fn set(&self, hive: Hive, key: &str, name: &str, value: Value) -> std::io::Result<()>;
    /// Deletes `key` and everything below it. Missing keys are fine.
    fn delete_tree(&self, hive: Hive, key: &str) -> std::io::Result<()>;
    /// Names of the direct subkeys of `key` (empty when missing).
    fn subkeys(&self, hive: Hive, key: &str) -> std::io::Result<Vec<String>>;
    /// Deletes one value. A missing key or value is fine.
    fn delete_value(&self, hive: Hive, key: &str, name: &str) -> std::io::Result<()>;
    /// Names of the values of `key` (empty when missing); `""` is the default value.
    fn value_names(&self, hive: Hive, key: &str) -> std::io::Result<Vec<String>>;
    /// Whether `key` exists.
    fn exists(&self, hive: Hive, key: &str) -> std::io::Result<bool>;

    fn get_string(&self, hive: Hive, key: &str, name: &str) -> Option<String> {
        match self.get(hive, key, name) {
            Ok(Some(Value::String(s))) => Some(s),
            _ => None,
        }
    }
}

/// Deletes `key` if it has neither values nor subkeys, then its parents the same way up to
/// (not including) `stop`. Used to tidy keys Stockroom may have created without ever deleting
/// a key someone else still uses.
pub fn delete_if_empty(reg: &dyn Registry, hive: Hive, key: &str, stop: &str) -> std::io::Result<()> {
    let stop = norm(stop);
    let mut k = key.trim_matches('\\').to_owned();
    while norm(&k) != stop && norm(&k).starts_with(&stop) {
        // A missing key has nothing to delete, but its parent may still be empty.
        if reg.exists(hive, &k)? {
            if !reg.value_names(hive, &k)?.is_empty() || !reg.subkeys(hive, &k)?.is_empty() {
                break;
            }
            reg.delete_tree(hive, &k)?;
        }
        match k.rsplit_once('\\') {
            Some((parent, _)) => k = parent.to_owned(),
            None => break,
        }
    }
    Ok(())
}

/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall`.
pub const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

fn norm(key: &str) -> String {
    key.trim_matches('\\').to_ascii_lowercase()
}

type Tree = BTreeMap<(Hive, String), BTreeMap<String, Value>>;

/// An in-memory registry, saved to `file` after each change when one is given.
#[derive(Debug, Default)]
pub struct MemRegistry {
    tree: Mutex<Tree>,
    /// Original-case key names, for `subkeys`.
    names: Mutex<BTreeMap<(Hive, String), String>>,
    file: Option<PathBuf>,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Saved {
    keys: Vec<(Hive, String, BTreeMap<String, Value>)>,
}

impl MemRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads (or starts) a registry persisted at `file`.
    pub fn persistent(file: &Path) -> std::io::Result<Self> {
        let reg = Self { file: Some(file.to_owned()), ..Self::default() };
        match std::fs::read(file) {
            Ok(bytes) => {
                let saved: Saved = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
                for (hive, key, values) in saved.keys {
                    for (name, value) in values {
                        reg.set(hive, &key, &name, value)?;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(reg)
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(file) = &self.file else { return Ok(()) };
        let tree = lock(&self.tree);
        let names = lock(&self.names);
        let keys = tree
            .iter()
            .map(|((h, k), v)| {
                (*h, names.get(&(*h, k.clone())).cloned().unwrap_or_else(|| k.clone()), v.clone())
            })
            .collect();
        let bytes = serde_json::to_vec_pretty(&Saved { keys }).map_err(std::io::Error::other)?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = file.with_extension("tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(tmp, file)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Registry for MemRegistry {
    fn get(&self, hive: Hive, key: &str, name: &str) -> std::io::Result<Option<Value>> {
        Ok(lock(&self.tree).get(&(hive, norm(key))).and_then(|v| v.get(&name.to_ascii_lowercase())).cloned())
    }

    fn set(&self, hive: Hive, key: &str, name: &str, value: Value) -> std::io::Result<()> {
        {
            let mut tree = lock(&self.tree);
            let mut names = lock(&self.names);
            // Create the key and its parents, as the real registry does.
            let trimmed = key.trim_matches('\\');
            let parts: Vec<&str> = trimmed.split('\\').collect();
            for i in 1..=parts.len() {
                let k = parts[..i].join("\\");
                tree.entry((hive, norm(&k))).or_default();
                names.entry((hive, norm(&k))).or_insert(k);
            }
            tree.entry((hive, norm(key))).or_default().insert(name.to_ascii_lowercase(), value);
        }
        self.save()
    }

    fn delete_tree(&self, hive: Hive, key: &str) -> std::io::Result<()> {
        {
            let k = norm(key);
            let prefix = format!("{k}\\");
            let keep = |h: &Hive, n: &String| !(*h == hive && (*n == k || n.starts_with(&prefix)));
            lock(&self.tree).retain(|(h, n), _| keep(h, n));
            lock(&self.names).retain(|(h, n), _| keep(h, n));
        }
        self.save()
    }

    fn subkeys(&self, hive: Hive, key: &str) -> std::io::Result<Vec<String>> {
        let prefix = format!("{}\\", norm(key));
        let names = lock(&self.names);
        Ok(names
            .iter()
            .filter(|((h, n), _)| *h == hive && n.starts_with(&prefix) && !n[prefix.len()..].contains('\\'))
            .filter_map(|(_, orig)| orig.rsplit('\\').next().map(str::to_owned))
            .collect())
    }

    fn delete_value(&self, hive: Hive, key: &str, name: &str) -> std::io::Result<()> {
        let removed = lock(&self.tree)
            .get_mut(&(hive, norm(key)))
            .and_then(|v| v.remove(&name.to_ascii_lowercase()))
            .is_some();
        if removed { self.save() } else { Ok(()) }
    }

    fn value_names(&self, hive: Hive, key: &str) -> std::io::Result<Vec<String>> {
        Ok(lock(&self.tree).get(&(hive, norm(key))).map_or_default(|v| v.keys().cloned().collect()))
    }

    fn exists(&self, hive: Hive, key: &str) -> std::io::Result<bool> {
        Ok(lock(&self.tree).contains_key(&(hive, norm(key))))
    }
}

/// The real registry.
#[cfg(windows)]
#[derive(Debug, Default, Clone, Copy)]
pub struct WinRegistry;

#[cfg(windows)]
impl WinRegistry {
    fn root(hive: Hive) -> winreg::RegKey {
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        winreg::RegKey::predef(match hive {
            Hive::CurrentUser => HKEY_CURRENT_USER,
            Hive::LocalMachine => HKEY_LOCAL_MACHINE,
        })
    }
}

#[cfg(windows)]
impl Registry for WinRegistry {
    fn get(&self, hive: Hive, key: &str, name: &str) -> std::io::Result<Option<Value>> {
        let k = match Self::root(hive).open_subkey(key) {
            Ok(k) => k,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if let Ok(s) = k.get_value::<String, _>(name) {
            return Ok(Some(Value::String(s)));
        }
        match k.get_value::<u32, _>(name) {
            Ok(d) => Ok(Some(Value::Dword(d))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Ok(None),
        }
    }

    fn set(&self, hive: Hive, key: &str, name: &str, value: Value) -> std::io::Result<()> {
        if hive != Hive::CurrentUser {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Stockroom only writes HKCU",
            ));
        }
        let (k, _) = Self::root(hive).create_subkey(key)?;
        match value {
            Value::String(s) => k.set_value(name, &s),
            Value::Dword(d) => k.set_value(name, &d),
        }
    }

    fn delete_tree(&self, hive: Hive, key: &str) -> std::io::Result<()> {
        if hive != Hive::CurrentUser {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Stockroom only writes HKCU",
            ));
        }
        match Self::root(hive).delete_subkey_all(key) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    fn subkeys(&self, hive: Hive, key: &str) -> std::io::Result<Vec<String>> {
        match Self::root(hive).open_subkey(key) {
            Ok(k) => Ok(k.enum_keys().filter_map(Result::ok).collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn delete_value(&self, hive: Hive, key: &str, name: &str) -> std::io::Result<()> {
        if hive != Hive::CurrentUser {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Stockroom only writes HKCU",
            ));
        }
        let k = match Self::root(hive).open_subkey_with_flags(key, winreg::enums::KEY_SET_VALUE) {
            Ok(k) => k,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        match k.delete_value(name) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    fn value_names(&self, hive: Hive, key: &str) -> std::io::Result<Vec<String>> {
        match Self::root(hive).open_subkey(key) {
            Ok(k) => Ok(k.enum_values().filter_map(Result::ok).map(|(name, _)| name).collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn exists(&self, hive: Hive, key: &str) -> std::io::Result<bool> {
        match Self::root(hive).open_subkey(key) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_registry_behaves_like_the_registry() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("reg.json");
        let r = MemRegistry::persistent(&file).unwrap();
        let key = format!(r"{UNINSTALL_KEY}\Stockroom.specular");
        r.set(Hive::CurrentUser, &key, "DisplayName", Value::String("Specular".into())).unwrap();
        r.set(Hive::CurrentUser, &key, "NoModify", Value::Dword(1)).unwrap();
        assert_eq!(
            r.get_string(Hive::CurrentUser, &key.to_uppercase(), "displayname").as_deref(),
            Some("Specular")
        );
        assert_eq!(r.subkeys(Hive::CurrentUser, UNINSTALL_KEY).unwrap(), ["Stockroom.specular"]);
        assert_eq!(r.subkeys(Hive::LocalMachine, UNINSTALL_KEY).unwrap(), [""; 0]);

        let again = MemRegistry::persistent(&file).unwrap();
        assert_eq!(again.get(Hive::CurrentUser, &key, "NoModify").unwrap(), Some(Value::Dword(1)));
        again.delete_tree(Hive::CurrentUser, &key).unwrap();
        assert_eq!(again.subkeys(Hive::CurrentUser, UNINSTALL_KEY).unwrap(), [""; 0]);
        assert!(
            MemRegistry::persistent(&file)
                .unwrap()
                .get(Hive::CurrentUser, &key, "NoModify")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn values_and_empty_keys() {
        let r = MemRegistry::new();
        // Values go one by one; empty keys are tidied up to a stop key, never past it.
        let ext = r"Software\Classes\.demo\OpenWithProgids";
        r.set(Hive::CurrentUser, ext, "Stockroom.demo", Value::String(String::new())).unwrap();
        assert_eq!(r.value_names(Hive::CurrentUser, ext).unwrap(), ["stockroom.demo"]);
        r.delete_value(Hive::CurrentUser, ext, "Stockroom.demo").unwrap();
        r.delete_value(Hive::CurrentUser, ext, "Stockroom.demo").unwrap();
        assert!(r.exists(Hive::CurrentUser, ext).unwrap());
        delete_if_empty(&r, Hive::CurrentUser, ext, r"Software\Classes").unwrap();
        assert!(!r.exists(Hive::CurrentUser, r"Software\Classes\.demo").unwrap());
        assert!(r.exists(Hive::CurrentUser, r"Software\Classes").unwrap());
        // A key with someone else's value stays.
        r.set(Hive::CurrentUser, ext, "Other.app", Value::String(String::new())).unwrap();
        delete_if_empty(&r, Hive::CurrentUser, ext, r"Software\Classes").unwrap();
        assert!(r.exists(Hive::CurrentUser, ext).unwrap());
    }
}
