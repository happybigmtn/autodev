use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::util::write_0o600_if_unix;

const CONFIG_DIR: &str = "quota-router";
const CONFIG_FILE: &str = "config.toml";
const PROFILES_DIR: &str = "profiles";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub(crate) fn auth_source(self) -> PathBuf {
        let home = dirs::home_dir().expect("cannot resolve home directory");
        match self {
            Self::Claude => home.join(".claude"),
            Self::Codex => home.join(".codex").join("auth.json"),
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl std::str::FromStr for Provider {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            other => bail!("unknown provider '{other}', expected 'claude' or 'codex'"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AccountEntry {
    pub(crate) name: String,
    pub(crate) provider: Provider,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct QuotaConfig {
    #[serde(default)]
    pub(crate) accounts: Vec<AccountEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selected_codex_account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selected_claude_account: Option<String>,
}

impl QuotaConfig {
    pub(crate) fn config_dir() -> PathBuf {
        let base = dirs::config_dir().expect("cannot resolve config directory");
        base.join(CONFIG_DIR)
    }

    pub(crate) fn config_path() -> PathBuf {
        Self::config_dir().join(CONFIG_FILE)
    }

    pub(crate) fn profiles_dir() -> PathBuf {
        Self::config_dir().join(PROFILES_DIR)
    }

    pub(crate) fn profile_dir(provider: Provider, name: &str) -> PathBuf {
        Self::profiles_dir().join(format!("{}-{name}", provider.label()))
    }

    pub(crate) fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
    }

    pub(crate) fn load_or_none() -> Result<Option<Self>> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(None);
        }
        Self::load().map(Some)
    }

    pub(crate) fn save(&self) -> Result<()> {
        let path = Self::config_path();
        let dir = Self::config_dir();
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        let text = toml::to_string_pretty(self).context("failed to serialize quota config")?;
        write_0o600_if_unix(&path, text.as_bytes())
    }

    pub(crate) fn find_account(&self, name: &str) -> Option<&AccountEntry> {
        self.accounts.iter().find(|a| a.name == name)
    }

    pub(crate) fn accounts_for_provider(&self, provider: Provider) -> Vec<&AccountEntry> {
        self.accounts
            .iter()
            .filter(|a| a.provider == provider)
            .collect()
    }

    pub(crate) fn selected_account_name(&self, provider: Provider) -> Option<&str> {
        match provider {
            Provider::Codex => self.selected_codex_account.as_deref(),
            Provider::Claude => self.selected_claude_account.as_deref(),
        }
    }

    pub(crate) fn set_selected_account(&mut self, provider: Provider, name: &str) -> Result<()> {
        if !self
            .accounts
            .iter()
            .any(|a| a.provider == provider && a.name == name)
        {
            bail!("account '{name}' not found for provider '{provider}'");
        }

        match provider {
            Provider::Codex => self.selected_codex_account = Some(name.to_owned()),
            Provider::Claude => self.selected_claude_account = Some(name.to_owned()),
        }
        Ok(())
    }

    pub(crate) fn clear_selected_account_if_matches(&mut self, provider: Provider, name: &str) {
        let selected = match provider {
            Provider::Codex => &mut self.selected_codex_account,
            Provider::Claude => &mut self.selected_claude_account,
        };
        if selected.as_deref() == Some(name) {
            *selected = None;
        }
    }

    pub(crate) fn add_account(&mut self, entry: AccountEntry) -> Result<()> {
        if self.accounts.iter().any(|a| a.name == entry.name) {
            bail!("account '{}' already exists", entry.name);
        }
        let provider = entry.provider;
        let name = entry.name.clone();
        self.accounts.push(entry);
        if self.selected_account_name(provider).is_none() {
            self.set_selected_account(provider, &name)?;
        }
        Ok(())
    }

    pub(crate) fn remove_account(&mut self, name: &str) -> Result<AccountEntry> {
        let idx = self
            .accounts
            .iter()
            .position(|a| a.name == name)
            .with_context(|| format!("account '{name}' not found"))?;
        let removed = self.accounts.remove(idx);
        self.clear_selected_account_if_matches(removed.provider, name);
        Ok(removed)
    }
}

pub(crate) fn copy_auth_to_profile(provider: Provider, profile_dir: &Path) -> Result<()> {
    let staged_profile_dir = staged_profile_dir(profile_dir)?;
    if staged_profile_dir.exists() {
        remove_profile_dir(&staged_profile_dir)?;
    }
    fs::create_dir_all(&staged_profile_dir)
        .with_context(|| format!("failed to create {}", staged_profile_dir.display()))?;

    let capture_result = (|| -> Result<()> {
        let source = provider.auth_source();
        match provider {
            Provider::Codex => {
                if missing_path(&source)? {
                    bail!(
                        "codex auth file not found at {}. Log in with `codex` first.",
                        source.display()
                    );
                }
                copy_credential_file(&source, &staged_profile_dir.join("auth.json"))
            }
            Provider::Claude => {
                if missing_path(&source)? {
                    bail!(
                        "claude config directory not found at {}. Log in with `claude` first.",
                        source.display()
                    );
                }
                ensure_credential_dir(&source)?;
                for filename in &[".credentials.json", "credentials.json", "statsig"] {
                    let src = source.join(filename);
                    copy_optional_credential_path(&src, &staged_profile_dir.join(filename))?;
                }
                let home = dirs::home_dir().expect("cannot resolve home directory");
                let claude_json = home.join(".claude.json");
                copy_optional_credential_path(
                    &claude_json,
                    &staged_profile_dir.join(".claude.json"),
                )
            }
        }
    })();

    if let Err(error) = capture_result {
        let _ = fs::remove_dir_all(&staged_profile_dir);
        return Err(error);
    }

    replace_profile_dir(profile_dir, &staged_profile_dir)
}

fn staged_profile_dir(profile_dir: &Path) -> Result<PathBuf> {
    let parent = profile_dir
        .parent()
        .with_context(|| format!("{} has no parent directory", profile_dir.display()))?;
    let name = profile_dir
        .file_name()
        .with_context(|| format!("{} has no file name", profile_dir.display()))?
        .to_string_lossy();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time is before unix epoch")?
        .as_nanos();
    Ok(parent.join(format!(".{name}.capture-{}-{stamp}", std::process::id())))
}

fn replace_profile_dir(profile_dir: &Path, staged_profile_dir: &Path) -> Result<()> {
    remove_profile_dir(profile_dir)?;
    fs::rename(staged_profile_dir, profile_dir).with_context(|| {
        format!(
            "failed to replace {} with {}",
            profile_dir.display(),
            staged_profile_dir.display()
        )
    })
}

fn remove_profile_dir(path: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to stat {}", path.display()))
        }
    };
    if meta.file_type().is_symlink() {
        bail!(
            "refusing to replace symlinked profile directory {}",
            path.display()
        );
    }
    if !meta.is_dir() {
        bail!(
            "refusing to replace non-directory profile path {}",
            path.display()
        );
    }
    fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))
}

fn missing_path(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error).with_context(|| format!("failed to stat {}", path.display())),
    }
}

fn copy_optional_credential_path(src: &Path, dst: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(src) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to stat {}", src.display()))
        }
    };
    copy_credential_path_with_metadata(src, dst, &meta)
}

fn ensure_credential_dir(src: &Path) -> Result<()> {
    let meta =
        fs::symlink_metadata(src).with_context(|| format!("failed to stat {}", src.display()))?;
    if meta.file_type().is_symlink() {
        bail!(
            "refusing to copy symlinked credential path {}",
            src.display()
        );
    }
    if !meta.is_dir() {
        bail!(
            "refusing to copy non-directory credential path {}",
            src.display()
        );
    }
    Ok(())
}

fn copy_credential_path_with_metadata(src: &Path, dst: &Path, meta: &fs::Metadata) -> Result<()> {
    if meta.file_type().is_symlink() {
        bail!(
            "refusing to copy symlinked credential path {}",
            src.display()
        );
    }
    if meta.is_dir() {
        copy_dir_recursive(src, dst)
    } else if meta.is_file() {
        copy_credential_file(src, dst)
    } else {
        bail!(
            "refusing to copy non-regular credential path {}",
            src.display()
        );
    }
}

fn copy_credential_file(src: &Path, dst: &Path) -> Result<()> {
    let meta =
        fs::symlink_metadata(src).with_context(|| format!("failed to stat {}", src.display()))?;
    if meta.file_type().is_symlink() {
        bail!(
            "refusing to copy symlinked credential path {}",
            src.display()
        );
    }
    if !meta.is_file() {
        bail!(
            "refusing to copy non-regular credential path {}",
            src.display()
        );
    }
    let bytes = fs::read(src).with_context(|| format!("failed to read {}", src.display()))?;
    write_0o600_if_unix(dst, &bytes)
        .with_context(|| format!("failed to copy {} -> {}", src.display(), dst.display()))
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    let meta =
        fs::symlink_metadata(src).with_context(|| format!("failed to stat {}", src.display()))?;
    if meta.file_type().is_symlink() {
        bail!(
            "refusing to copy symlinked credential path {}",
            src.display()
        );
    }
    if !meta.is_dir() {
        bail!(
            "refusing to copy non-directory credential path {}",
            src.display()
        );
    }
    fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
    for entry in
        fs::read_dir(src).with_context(|| format!("failed to read directory {}", src.display()))?
    {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let meta = fs::symlink_metadata(&src_path)
            .with_context(|| format!("failed to stat {}", src_path.display()))?;
        copy_credential_path_with_metadata(&src_path, &dst_path, &meta)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::sync::MutexGuard;
    #[cfg(unix)]
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    struct TempConfigHome {
        root: PathBuf,
        previous: Option<OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    #[cfg(unix)]
    impl TempConfigHome {
        fn new(label: &str) -> Self {
            let lock = crate::util::test_process_env_lock()
                .lock()
                .expect("failed to lock process env");
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("autodev-{label}-{}-{stamp}", std::process::id()));
            fs::create_dir_all(&root).expect("failed to create temp config root");
            let previous = std::env::var_os("XDG_CONFIG_HOME");
            std::env::set_var("XDG_CONFIG_HOME", &root);
            Self {
                root,
                previous,
                _lock: lock,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for TempConfigHome {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var("XDG_CONFIG_HOME", previous);
            } else {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[cfg(unix)]
    struct TempQuotaHome {
        root: PathBuf,
        home_previous: Option<OsString>,
        config_previous: Option<OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    #[cfg(unix)]
    impl TempQuotaHome {
        fn new(label: &str) -> Self {
            let lock = crate::util::test_process_env_lock()
                .lock()
                .expect("failed to lock process env");
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("autodev-{label}-{}-{stamp}", std::process::id()));
            let home = root.join("home");
            let config = root.join("config");
            fs::create_dir_all(&home).expect("failed to create temp home");
            fs::create_dir_all(&config).expect("failed to create temp config");
            let home_previous = std::env::var_os("HOME");
            let config_previous = std::env::var_os("XDG_CONFIG_HOME");
            std::env::set_var("HOME", &home);
            std::env::set_var("XDG_CONFIG_HOME", &config);
            Self {
                root,
                home_previous,
                config_previous,
                _lock: lock,
            }
        }

        fn home(&self) -> PathBuf {
            self.root.join("home")
        }

        fn profile_dir(&self, provider: Provider, name: &str) -> PathBuf {
            self.root
                .join("config")
                .join("quota-router")
                .join("profiles")
                .join(format!("{}-{name}", provider.label()))
        }
    }

    #[cfg(unix)]
    impl Drop for TempQuotaHome {
        fn drop(&mut self) {
            if let Some(previous) = &self.home_previous {
                std::env::set_var("HOME", previous);
            } else {
                std::env::remove_var("HOME");
            }
            if let Some(previous) = &self.config_previous {
                std::env::set_var("XDG_CONFIG_HOME", previous);
            } else {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn parse_config_round_trip() {
        let config = QuotaConfig {
            accounts: vec![
                AccountEntry {
                    name: "work-codex".into(),
                    provider: Provider::Codex,
                },
                AccountEntry {
                    name: "personal-claude".into(),
                    provider: Provider::Claude,
                },
            ],
            selected_codex_account: Some("work-codex".into()),
            selected_claude_account: Some("personal-claude".into()),
        };
        let text = toml::to_string_pretty(&config).unwrap();
        let parsed: QuotaConfig = toml::from_str(&text).unwrap();
        assert_eq!(parsed.accounts.len(), 2);
        assert_eq!(parsed.accounts[0].name, "work-codex");
        assert_eq!(parsed.accounts[1].provider, Provider::Claude);
    }

    #[test]
    fn duplicate_account_rejected() {
        let mut config = QuotaConfig::default();
        let entry = AccountEntry {
            name: "test".into(),
            provider: Provider::Codex,
        };
        config.add_account(entry.clone()).unwrap();
        assert!(config
            .add_account(AccountEntry {
                name: "test".into(),
                provider: Provider::Codex,
            })
            .is_err());
    }

    #[test]
    fn remove_nonexistent_account_errors() {
        let mut config = QuotaConfig::default();
        assert!(config.remove_account("nonexistent").is_err());
    }

    #[test]
    fn accounts_for_provider_filters() {
        let config = QuotaConfig {
            accounts: vec![
                AccountEntry {
                    name: "c1".into(),
                    provider: Provider::Codex,
                },
                AccountEntry {
                    name: "cl1".into(),
                    provider: Provider::Claude,
                },
                AccountEntry {
                    name: "c2".into(),
                    provider: Provider::Codex,
                },
            ],
            selected_codex_account: Some("c1".into()),
            selected_claude_account: Some("cl1".into()),
        };
        let codex_accounts = config.accounts_for_provider(Provider::Codex);
        assert_eq!(codex_accounts.len(), 2);
        let claude_accounts = config.accounts_for_provider(Provider::Claude);
        assert_eq!(claude_accounts.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn capture_writes_codex_auth_owner_only() {
        let home = TempQuotaHome::new("quota-config-codex-capture");
        let codex_dir = home.home().join(".codex");
        fs::create_dir_all(&codex_dir).expect("failed to create codex auth dir");
        fs::write(codex_dir.join("auth.json"), br#"{"account":"active"}"#)
            .expect("failed to write codex auth");

        let profile_dir = home.profile_dir(Provider::Codex, "work");
        fs::create_dir_all(&profile_dir).expect("failed to create stale profile dir");
        fs::write(profile_dir.join("stale.json"), br#"{"account":"stale"}"#)
            .expect("failed to write stale profile file");

        copy_auth_to_profile(Provider::Codex, &profile_dir).expect("codex capture should succeed");

        let entries = fs::read_dir(&profile_dir)
            .expect("failed to read profile dir")
            .map(|entry| entry.expect("failed to read profile entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![OsString::from("auth.json")]);

        let profile_auth = profile_dir.join("auth.json");
        let meta = fs::symlink_metadata(&profile_auth).expect("failed to stat profile auth");
        assert!(meta.is_file());
        assert!(!meta.file_type().is_symlink());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn capture_rejects_symlinked_codex_auth() {
        let home = TempQuotaHome::new("quota-config-symlink-codex");
        let codex_dir = home.home().join(".codex");
        fs::create_dir_all(&codex_dir).expect("failed to create codex auth dir");
        let real_auth = home.home().join("real-auth.json");
        fs::write(&real_auth, br#"{"account":"linked"}"#).expect("failed to write real auth");
        let linked_auth = codex_dir.join("auth.json");
        std::os::unix::fs::symlink(&real_auth, &linked_auth).expect("failed to symlink auth");

        let profile_dir = home.profile_dir(Provider::Codex, "work");
        let error = copy_auth_to_profile(Provider::Codex, &profile_dir)
            .expect_err("symlinked codex auth should be rejected")
            .to_string();

        assert!(error.contains("symlinked credential path"));
        assert!(error.contains(&linked_auth.display().to_string()));
        assert!(!profile_dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn capture_prunes_stale_profile_files() {
        let home = TempQuotaHome::new("quota-config-stale-profile");
        let claude_dir = home.home().join(".claude");
        fs::create_dir_all(&claude_dir).expect("failed to create claude config dir");
        fs::write(
            claude_dir.join(".credentials.json"),
            br#"{"account":"active"}"#,
        )
        .expect("failed to write claude credentials");
        fs::write(
            claude_dir.join("credentials.json"),
            br#"{"account":"stale-next"}"#,
        )
        .expect("failed to write removable claude credentials");
        fs::write(home.home().join(".claude.json"), br#"{"home":"json"}"#)
            .expect("failed to write claude home json");

        let profile_dir = home.profile_dir(Provider::Claude, "work");
        copy_auth_to_profile(Provider::Claude, &profile_dir)
            .expect("initial claude capture should succeed");
        assert!(profile_dir.join("credentials.json").exists());
        assert!(profile_dir.join(".claude.json").exists());

        fs::remove_file(claude_dir.join("credentials.json"))
            .expect("failed to remove active credentials source");
        fs::remove_file(home.home().join(".claude.json"))
            .expect("failed to remove claude home json source");

        copy_auth_to_profile(Provider::Claude, &profile_dir).expect("recapture should succeed");

        assert!(profile_dir.join(".credentials.json").exists());
        assert!(!profile_dir.join("credentials.json").exists());
        assert!(!profile_dir.join(".claude.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_owner_only() {
        let _config_home = TempConfigHome::new("quota-config-save");
        let config = QuotaConfig {
            accounts: vec![AccountEntry {
                name: "work-codex".into(),
                provider: Provider::Codex,
            }],
            selected_codex_account: Some("work-codex".into()),
            selected_claude_account: None,
        };

        config.save().expect("config save should succeed");

        let mode = fs::metadata(QuotaConfig::config_path())
            .expect("failed to stat saved config")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
