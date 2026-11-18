use super::*;
use anyhow::bail;
use std::{cell::RefCell, collections::HashMap};

#[derive(Default)]
pub(crate) struct MemoryVault {
    pub values: RefCell<HashMap<String, String>>,
    pub fail: RefCell<Option<String>>,
}
impl Vault for MemoryVault {
    fn get(&self, target: &str) -> Result<Option<String>> {
        Ok(self.values.borrow().get(target).cloned())
    }
    fn set(&self, target: &str, value: Option<&str>) -> Result<()> {
        if self.fail.borrow().as_deref() == Some(target) {
            bail!("Simulated vault failure");
        }
        match value {
            Some(value) => {
                self.values.borrow_mut().insert(target.into(), value.into());
            }
            None => {
                self.values.borrow_mut().remove(target);
            }
        }
        Ok(())
    }
}

#[test]
fn saving_failure_rolls_back_key_changes_and_deletions() {
    let vault = MemoryVault::default();
    vault.set("a", Some("old-key")).unwrap();
    vault.set("b", Some("other-key")).unwrap();
    let before = vault.values.borrow().clone();
    let changes = vec![
        ("a".into(), "new-key".into()),
        ("b".into(), String::new()),
        ("c".into(), "third-key".into()),
    ];
    assert!(transaction(&vault, &changes, || bail!("Disk full")).is_err());
    assert!(before == *vault.values.borrow());
    *vault.fail.borrow_mut() = Some("b".into());
    assert!(transaction(&vault, &changes, || Ok(())).is_err());
    assert!(before == *vault.values.borrow());
}

#[test]
fn file_credentials_survive_restart_clear_explicitly_and_restrict_permissions() {
    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(uuid::Uuid::new_v4().to_string());
    let path = directory.join("credentials.json");
    let vault = FileVault::isolated(path.clone());
    vault.set("api/provider", Some("test-only-key")).unwrap();
    vault
        .set("sync/password", Some("test-only-password"))
        .unwrap();
    let reopened = FileVault::isolated(path.clone());
    assert_eq!(
        reopened.get("api/provider").unwrap().as_deref(),
        Some("test-only-key")
    );
    reopened.set("api/provider", None).unwrap();
    assert!(
        FileVault::isolated(path.clone())
            .get("api/provider")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        reopened.get("sync/password").unwrap().as_deref(),
        Some("test-only-password")
    );
    reopened
        .finish_migration(&["sync/password".into(), "missing-legacy-key".into()])
        .unwrap();
    assert_eq!(
        reopened.get("sync/password").unwrap().as_deref(),
        Some("test-only-password")
    );
    assert!(reopened.get("missing-legacy-key").unwrap().is_none());
    assert!(
        transaction(
            &reopened,
            &[("sync/password".into(), "replacement".into())],
            || bail!("Settings write failed")
        )
        .is_err()
    );
    assert_eq!(
        reopened.get("sync/password").unwrap().as_deref(),
        Some("test-only-password")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    std::fs::write(&path, b"{corrupted").unwrap();
    assert!(reopened.get("api/provider").is_err());
    assert!(reopened.set("api/provider", Some("replacement")).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"{corrupted");
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
#[ignore = "Uses the current local credential profile; isolated coverage runs by default"]
#[cfg(any(windows, target_os = "macos"))]
fn system_vault_roundtrip() {
    let target = format!("pordo/test/{}", uuid::Uuid::new_v4());
    struct Cleanup(String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = SystemVault.set(&self.0, None);
        }
    }
    let _cleanup = Cleanup(target.clone());
    assert!(SystemVault.get(&target).unwrap().is_none());
    SystemVault.set(&target, Some("test-only-key")).unwrap();
    assert!(SystemVault.get(&target).unwrap().as_deref() == Some("test-only-key"));
    SystemVault
        .set(&target, Some("replacement-test-key"))
        .unwrap();
    assert!(SystemVault.get(&target).unwrap().as_deref() == Some("replacement-test-key"));
    SystemVault.set(&target, None).unwrap();
    assert!(SystemVault.get(&target).unwrap().is_none());
}
