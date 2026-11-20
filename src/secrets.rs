//! Host passwords and tokens live in the OS credential store (Windows
//! Credential Manager, macOS Keychain, Linux keyutils), never in config.toml.
//!
//! Secrets saved before the project was renamed live under the old service
//! name; they are moved over the first time they are read.

const SERVICE: &str = "godrays";
const OLD_SERVICE: &str = "kyber-launcher";

/// Credential-store name of the password others use to join this PC.
pub const HOSTING: &str = "hosting";

fn host_name(host_id: u64) -> String {
    format!("host-{host_id}")
}

fn entry_in(service: &str, name: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(service, name).map_err(|e| e.to_string())
}

fn read(service: &str, name: &str) -> Result<Option<String>, String> {
    match entry_in(service, name)?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn remove(service: &str, name: &str) -> Result<(), String> {
    match entry_in(service, name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Reads `name`, moving it from the old service name if that is where it is.
fn get_migrating(name: &str, old_service: &str) -> Result<Option<String>, String> {
    if let Some(secret) = read(SERVICE, name)? {
        return Ok(Some(secret));
    }
    let Ok(Some(secret)) = read(old_service, name) else {
        return Ok(None);
    };
    entry_in(SERVICE, name)?
        .set_password(&secret)
        .map_err(|e| e.to_string())?;
    // Only once the copy is safely stored.
    let _ = remove(old_service, name);
    Ok(Some(secret))
}

/// Like [`get`], for a launcher-wide secret such as [`HOSTING`].
pub fn get_named(name: &str) -> Result<Option<String>, String> {
    get_migrating(name, OLD_SERVICE)
}

/// Removes a launcher-wide secret (from both service names).
pub fn delete_named(name: &str) -> Result<(), String> {
    remove(SERVICE, name)?;
    remove(OLD_SERVICE, name)
}

pub fn set_named(name: &str, secret: &str) -> Result<(), String> {
    entry_in(SERVICE, name)?
        .set_password(secret)
        .map_err(|e| e.to_string())
}

/// Returns the stored secret, or `None` when there is none.
pub fn get(host_id: u64) -> Result<Option<String>, String> {
    get_migrating(&host_name(host_id), OLD_SERVICE)
}

/// Stores the secret; an empty secret removes the entry.
pub fn set(host_id: u64, secret: &str) -> Result<(), String> {
    if secret.is_empty() {
        return delete(host_id);
    }
    entry_in(SERVICE, &host_name(host_id))?
        .set_password(secret)
        .map_err(|e| e.to_string())
}

pub fn delete(host_id: u64) -> Result<(), String> {
    remove(SERVICE, &host_name(host_id))?;
    remove(OLD_SERVICE, &host_name(host_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round trip through the real OS credential store, under an id no saved
    /// host can have; the entry is removed again at the end.
    #[test]
    fn secrets_round_trip_through_the_credential_store() {
        let id = u64::MAX - 1;
        delete(id).unwrap();
        assert_eq!(get(id).unwrap(), None);
        set(id, "p@ss word").unwrap();
        assert_eq!(get(id).unwrap().as_deref(), Some("p@ss word"));
        set(id, "").unwrap();
        assert_eq!(get(id).unwrap(), None);
    }

    /// A secret stored under a (test-only) old service name moves to the
    /// new one on first read, and is gone from the old one afterwards.
    #[test]
    fn secrets_saved_under_the_old_name_are_moved_over() {
        let old = "godrays-test-old-service";
        let name = "migration-test";
        remove(SERVICE, name).unwrap();
        entry_in(old, name)
            .unwrap()
            .set_password("from before")
            .unwrap();

        assert_eq!(
            get_migrating(name, old).unwrap().as_deref(),
            Some("from before")
        );
        assert_eq!(read(old, name).unwrap(), None);
        assert_eq!(read(SERVICE, name).unwrap().as_deref(), Some("from before"));

        remove(SERVICE, name).unwrap();
    }
}
