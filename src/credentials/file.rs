use super::{LegacyVault, Result, Vault};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    sync::Mutex,
};

static ACCESS: Mutex<()> = Mutex::new(());
const MAX_BYTES: usize = 2 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    version: u32,
    entries: BTreeMap<String, Option<String>>,
}
pub(crate) struct FileVault {
    path: PathBuf,
    migrate: bool,
}
impl FileVault {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            migrate: true,
        }
    }
    #[cfg(test)]
    pub(crate) fn isolated(path: PathBuf) -> Self {
        Self {
            path,
            migrate: false,
        }
    }
    fn edit<T>(&self, action: impl FnOnce(&mut Data) -> Result<(T, bool)>) -> Result<T> {
        let _access = ACCESS
            .lock()
            .map_err(|_| anyhow::anyhow!("Credential access lock failed"))?;
        let parent = self
            .path
            .parent()
            .context("Missing credentials directory")?;
        fs::create_dir_all(parent)?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(self.path.with_extension("lock"))?;
        lock.try_lock()
            .context("凭据文件正在使用，请稍后重试 / Credentials are in use; try again")?;
        let mut data = match fs::File::open(&self.path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
                ensure!(
                    bytes.len() <= MAX_BYTES,
                    "Credentials file exceeds size limit"
                );
                serde_json::from_slice::<Data>(&bytes).context("本机凭据文件无效，原文件已保留 / Invalid local credentials; original file preserved")?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Data {
                version: 1,
                entries: BTreeMap::new(),
            },
            Err(e) => return Err(e.into()),
        };
        ensure!(data.version == 1, "Unsupported credentials file version");
        let (value, changed) = action(&mut data)?;
        if changed {
            let bytes = serde_json::to_vec_pretty(&data)?;
            ensure!(
                bytes.len() <= MAX_BYTES,
                "Credentials file exceeds size limit"
            );
            let temp = self
                .path
                .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
            let result = (|| -> Result<()> {
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(&temp)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                drop(file);
                fs::rename(&temp, &self.path)?;
                ensure!(
                    fs::read(&self.path)? == bytes,
                    "Credential write verification failed"
                );
                Ok(())
            })();
            if result.is_err() {
                let _ = fs::remove_file(temp);
            }
            result?;
        }
        Ok(value)
    }
    pub(crate) fn stored_targets(&self) -> Result<BTreeSet<String>> {
        self.edit(|data| Ok((data.entries.keys().cloned().collect(), false)))
    }
}
impl Vault for FileVault {
    fn get(&self, target: &str) -> Result<Option<String>> {
        self.edit(|data| {
            if let Some(value) = data.entries.get(target) {
                return Ok((value.clone(), false));
            }
            let value = if self.migrate {
                LegacyVault.get(target)?
            } else {
                None
            };
            // A missing read isn't a deletion. Only store a value we could import.
            let imported = value.is_some();
            if imported {
                data.entries.insert(target.into(), value.clone());
            }
            Ok((value, imported))
        })
    }
    fn set(&self, target: &str, value: Option<&str>) -> Result<()> {
        self.edit(|data| {
            let value = value.map(str::to_owned);
            let changed = data.entries.get(target) != Some(&value);
            data.entries.insert(target.into(), value);
            Ok(((), changed))
        })
    }
}
