//! Local credentials live in a private JSON file, separate from settings and sync payloads.
use anyhow::{Context, Result};

pub trait Vault {
    fn get(&self, target: &str) -> Result<Option<String>>;
    fn set(&self, target: &str, value: Option<&str>) -> Result<()>;
}

pub struct SystemVault;
struct LegacyVault;
mod file;
pub(crate) use file::FileVault;

impl SystemVault {
    pub(crate) fn local() -> FileVault {
        FileVault::new(crate::settings::Settings::queue_path().with_file_name("credentials.json"))
    }
}
impl Vault for SystemVault {
    fn get(&self, target: &str) -> Result<Option<String>> {
        Self::local().get(target)
    }
    fn set(&self, target: &str, value: Option<&str>) -> Result<()> {
        Self::local().set(target, value)
    }
}

/// Roll back key changes if saving settings fails, so the applied config stays usable.
pub fn transaction(
    vault: &impl Vault,
    changes: &[(String, String)],
    commit: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let previous: Vec<_> = changes
        .iter()
        .map(|(target, _)| vault.get(target))
        .collect::<Result<_>>()?;
    for (index, (target, value)) in changes.iter().enumerate() {
        let next = (!value.is_empty()).then_some(value.as_str());
        if previous[index].as_deref() != next
            && let Err(error) = vault.set(target, next)
        {
            rollback(vault, changes, &previous, index)?;
            return Err(error);
        }
    }
    if let Err(error) = commit() {
        rollback(vault, changes, &previous, changes.len())?;
        return Err(error);
    }
    Ok(())
}

fn rollback(
    vault: &impl Vault,
    changes: &[(String, String)],
    previous: &[Option<String>],
    count: usize,
) -> Result<()> {
    for index in (0..count).rev() {
        vault
            .set(&changes[index].0, previous[index].as_deref())
            .context("密钥保存失败，无法恢复原配置 / Cannot restore previous credentials")?;
    }
    Ok(())
}

#[cfg(windows)]
impl LegacyVault {
    fn get(&self, target: &str) -> Result<Option<String>> {
        use windows_sys::Win32::{
            Foundation::{ERROR_NOT_FOUND, GetLastError},
            Security::Credentials::*,
        };
        let target: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
        let mut credential = std::ptr::null_mut();
        // Pointers refer to live UTF-16 input and an output owned by CredFree.
        unsafe {
            if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) == 0 {
                let error = GetLastError();
                if error == ERROR_NOT_FOUND {
                    return Ok(None);
                }
                anyhow::bail!("无法读取已保存的密钥 / Cannot read saved API key: {error}");
            }
            let blob = &*credential;
            let result = if blob.CredentialBlobSize == 0 {
                Ok(String::new())
            } else {
                String::from_utf8(
                    std::slice::from_raw_parts(
                        blob.CredentialBlob,
                        blob.CredentialBlobSize as usize,
                    )
                    .to_vec(),
                )
                .context("已保存的密钥格式无效 / Invalid saved API key")
            };
            CredFree(credential.cast());
            result.map(Some)
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use security_framework::passwords::{self, PasswordOptions};
    // Security.framework's errSecItemNotFound.
    const ITEM_NOT_FOUND: i32 = -25300;

    fn options(target: &str) -> PasswordOptions {
        // Published Keychain service: changing this would hide existing credentials.
        let mut options = PasswordOptions::new_generic_password("dev.replayer.credentials", target);
        options.set_access_synchronized(Some(false));
        options
    }
    impl LegacyVault {
        pub(super) fn get(&self, target: &str) -> Result<Option<String>> {
            // Migration must never display a Keychain authorization dialog.
            let _interaction =
                security_framework::os::macos::keychain::SecKeychain::disable_user_interaction()
                    .context("无法检查旧凭据 / Cannot inspect legacy credentials")?;
            match passwords::generic_password(options(target)) {
                Ok(bytes) => String::from_utf8(bytes)
                    .map(Some)
                    .context("已保存的凭据格式无效 / Invalid saved credential"),
                Err(error) if error.code() == ITEM_NOT_FOUND => Ok(None),
                Err(_) => anyhow::bail!(
                    "旧密钥仍保存在钥匙串，请在设置中选择重新填写密钥 / Legacy keys remain in Keychain; choose Re-enter keys in Settings"
                ),
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
impl LegacyVault {
    fn get(&self, _target: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

#[cfg(test)]
pub(crate) mod tests;
