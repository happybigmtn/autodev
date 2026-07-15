use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::util::{atomic_write_0o600_if_unix, write_0o600_if_unix};

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
    #[serde(default)]
    pub(crate) live: bool,
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

    pub(crate) fn profile_dir(provider: Provider, name: &str) -> Result<PathBuf> {
        validate_account_name(name)?;
        let profiles_dir = Self::profiles_dir();
        let profile_dir = profiles_dir.join(format!("{}-{name}", provider.label()));
        ensure_profile_path_contained(&profiles_dir, &profile_dir)?;
        Ok(profile_dir)
    }

    pub(crate) fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Self =
            toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
        config.validate_account_names()?;
        Ok(config)
    }

    pub(crate) fn load_or_none() -> Result<Option<Self>> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(None);
        }
        Self::load().map(Some)
    }

    pub(crate) fn save(&self) -> Result<()> {
        self.validate_account_names()?;
        let path = Self::config_path();
        let dir = Self::config_dir();
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        let text = toml::to_string_pretty(self).context("failed to serialize quota config")?;
        atomic_write_0o600_if_unix(&path, text.as_bytes())
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
        validate_account_name(name)?;
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
        validate_account_name(&entry.name)?;
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
        validate_account_name(name)?;
        let idx = self
            .accounts
            .iter()
            .position(|a| a.name == name)
            .with_context(|| format!("account '{name}' not found"))?;
        let removed = self.accounts.remove(idx);
        self.clear_selected_account_if_matches(removed.provider, name);
        Ok(removed)
    }

    fn validate_account_names(&self) -> Result<()> {
        for account in &self.accounts {
            validate_account_name(&account.name)?;
        }
        if let Some(name) = self.selected_codex_account.as_deref() {
            validate_account_name(name)?;
        }
        if let Some(name) = self.selected_claude_account.as_deref() {
            validate_account_name(name)?;
        }
        Ok(())
    }
}

/// Resolve the CODEX_HOME for a quota profile, honoring the isolated
/// `<profile_dir>/codex-home/` subdir layout when present.
///
/// Isolated layout is required to avoid OpenAI's cross-account session
/// revocation: every codex login from the same `installation_id` revokes
/// the prior account's refresh token. Each profile having its own
/// CODEX_HOME (with its own `installation_id`) makes OpenAI treat the
/// accounts as separate devices.
pub(crate) fn codex_home_for_profile(profile_dir: &Path) -> PathBuf {
    let subdir = profile_dir.join("codex-home");
    if subdir.join("auth.json").exists() {
        subdir
    } else {
        profile_dir.to_path_buf()
    }
}

/// Resolve the user's live CODEX_HOME. Live accounts reference the real
/// `~/.codex` login directly, so they keep using the token Codex refreshes
/// during normal operation instead of going stale after token rotation.
pub(crate) fn codex_live_home() -> PathBuf {
    dirs::home_dir()
        .expect("cannot resolve home directory")
        .join(".codex")
}

/// True iff this Codex profile uses the isolated `codex-home/` layout.
/// Callers should skip the legacy `~/.codex/auth.json` file swap and
/// instead spawn codex with `CODEX_HOME=<profile_dir>/codex-home`.
pub(crate) fn codex_profile_uses_isolated_home(profile_dir: &Path) -> bool {
    profile_dir.join("codex-home").join("auth.json").exists()
}

pub(crate) fn prepare_codex_profile_login_home(profile_dir: &Path) -> Result<PathBuf> {
    ensure_profile_path_contained(&QuotaConfig::profiles_dir(), profile_dir)?;
    ensure_plain_directory(profile_dir)?;

    let codex_home = profile_dir.join("codex-home");
    ensure_plain_directory(&codex_home)?;

    let active_config = dirs::home_dir()
        .expect("cannot resolve home directory")
        .join(".codex")
        .join("config.toml");
    let profile_config = codex_home.join("config.toml");
    if missing_path(&profile_config)? {
        copy_optional_credential_path(&active_config, &profile_config)?;
    }

    Ok(codex_home)
}

pub(crate) fn copy_auth_to_profile(provider: Provider, profile_dir: &Path) -> Result<()> {
    ensure_profile_path_contained(&QuotaConfig::profiles_dir(), profile_dir)?;
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
                copy_codex_credentials_to_isolated_home(&source, &staged_profile_dir)
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

fn copy_codex_credentials_to_isolated_home(source_auth: &Path, profile_dir: &Path) -> Result<()> {
    let codex_home = profile_dir.join("codex-home");
    fs::create_dir_all(&codex_home)
        .with_context(|| format!("failed to create {}", codex_home.display()))?;
    copy_credential_file(source_auth, &codex_home.join("auth.json"))?;

    let source_dir = source_auth
        .parent()
        .with_context(|| format!("{} has no parent directory", source_auth.display()))?;
    {
        let filename = &"config.toml";
        let src = source_dir.join(filename);
        copy_optional_credential_path(&src, &codex_home.join(filename))?;
    }

    Ok(())
}

pub(crate) fn validate_account_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("quota account name cannot be empty");
    }
    let bytes = name.as_bytes();
    let starts_and_ends_with_alnum = bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric);
    let slug_chars = bytes
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-');

    if !starts_and_ends_with_alnum || !slug_chars {
        bail!(
            "invalid quota account name '{name}': use lowercase ASCII letters, digits, and '-' separators; start and end with a letter or digit"
        );
    }
    Ok(())
}

fn ensure_profile_path_contained(profiles_dir: &Path, profile_dir: &Path) -> Result<()> {
    let logical_root = normalize_logical_path(profiles_dir)?;
    let logical_profile = normalize_logical_path(profile_dir)?;
    if !logical_profile.starts_with(&logical_root) {
        bail!(
            "quota profile path {} escapes profile root {}",
            profile_dir.display(),
            profiles_dir.display()
        );
    }

    if let Ok(canonical_root) = fs::canonicalize(profiles_dir) {
        let canonical_profile = match fs::canonicalize(profile_dir) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let filename = profile_dir
                    .file_name()
                    .with_context(|| format!("{} has no file name", profile_dir.display()))?;
                canonical_root.join(filename)
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to canonicalize {}", profile_dir.display()))
            }
        };
        if !canonical_profile.starts_with(&canonical_root) {
            bail!(
                "quota profile path {} resolves outside profile root {}",
                profile_dir.display(),
                profiles_dir.display()
            );
        }
    }

    Ok(())
}

fn normalize_logical_path(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => bail!("path {} contains parent traversal", path.display()),
            Component::Normal(part) => normalized.push(part),
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
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

fn ensure_plain_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                bail!(
                    "refusing to use symlinked profile directory {}",
                    path.display()
                );
            }
            if !meta.is_dir() {
                bail!(
                    "refusing to use non-directory profile path {}",
                    path.display()
                );
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
        }
        Err(error) => Err(error).with_context(|| format!("failed to stat {}", path.display())),
    }
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
                    live: false,
                },
                AccountEntry {
                    name: "personal-claude".into(),
                    provider: Provider::Claude,
                    live: false,
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
    fn account_entry_live_round_trips_and_defaults_false() {
        let text = r#"
[[accounts]]
name = "live-codex"
provider = "codex"
live = true

[[accounts]]
name = "captured-codex"
provider = "codex"
"#;
        let parsed: QuotaConfig = toml::from_str(text).unwrap();

        assert!(parsed.accounts[0].live);
        assert!(!parsed.accounts[1].live);

        let serialized = toml::to_string_pretty(&parsed).unwrap();
        let reparsed: QuotaConfig = toml::from_str(&serialized).unwrap();
        assert!(reparsed.accounts[0].live);
        assert!(!reparsed.accounts[1].live);
    }

    #[cfg(unix)]
    #[test]
    fn load_rejects_unsafe_account_names() {
        let _config_home = TempConfigHome::new("quota-config-load-unsafe");
        let config_path = QuotaConfig::config_path();
        let config_dir = config_path
            .parent()
            .expect("config path should have parent");
        fs::create_dir_all(config_dir).expect("failed to create quota config dir");

        fs::write(
            &config_path,
            r#"
selected_codex_account = "work-codex"

[[accounts]]
name = "work-codex"
provider = "codex"
"#,
        )
        .expect("failed to write valid persisted config");
        let config = QuotaConfig::load().expect("valid persisted config should load");
        assert_eq!(config.accounts[0].name, "work-codex");
        assert_eq!(config.selected_codex_account.as_deref(), Some("work-codex"));

        fs::write(
            &config_path,
            r#"
[[accounts]]
name = "../escape"
provider = "codex"
"#,
        )
        .expect("failed to write unsafe persisted config");
        let error = QuotaConfig::load()
            .expect_err("unsafe persisted account name must be rejected")
            .to_string();

        assert!(error.contains("invalid quota account name '../escape'"));
        assert!(error.contains("lowercase ASCII letters"));
    }

    #[test]
    fn duplicate_account_rejected() {
        let mut config = QuotaConfig::default();
        let entry = AccountEntry {
            name: "test".into(),
            provider: Provider::Codex,
            live: false,
        };
        config.add_account(entry.clone()).unwrap();
        assert!(config
            .add_account(AccountEntry {
                name: "test".into(),
                provider: Provider::Codex,
                live: false,
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
                    live: false,
                },
                AccountEntry {
                    name: "cl1".into(),
                    provider: Provider::Claude,
                    live: false,
                },
                AccountEntry {
                    name: "c2".into(),
                    provider: Provider::Codex,
                    live: false,
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
    fn profile_dir_rejects_unsafe_names() {
        let _home = TempConfigHome::new("quota-config-unsafe-names");
        let unsafe_names = [
            "",
            "../x",
            "/abs",
            "codex/work",
            "bad.name",
            "bad_name",
            "UPPER",
            "-leading",
            "trailing-",
        ];

        for name in unsafe_names {
            assert!(
                validate_account_name(name).is_err(),
                "{name:?} should fail slug validation"
            );
            assert!(
                QuotaConfig::profile_dir(Provider::Codex, name).is_err(),
                "{name:?} should not produce a profile path"
            );

            let mut config = QuotaConfig::default();
            let entry = AccountEntry {
                name: name.to_owned(),
                provider: Provider::Codex,
                live: false,
            };
            assert!(
                config.add_account(entry).is_err(),
                "{name:?} should not enter quota config"
            );
            assert!(config.accounts.is_empty());
            assert!(config.selected_codex_account.is_none());

            let mut state = crate::quota_state::QuotaState::default();
            assert!(
                state.mark_selected(name, chrono::Utc::now()).is_err(),
                "{name:?} should not enter quota state"
            );
            assert!(state.accounts.is_empty());
        }

        assert!(!QuotaConfig::config_path().exists());
        assert!(!QuotaConfig::profiles_dir().exists());
    }

    #[cfg(unix)]
    #[test]
    fn profile_dir_stays_under_profiles_root() {
        let home = TempConfigHome::new("quota-config-profile-containment");

        let profile_dir = QuotaConfig::profile_dir(Provider::Codex, "work-1")
            .expect("safe slug should produce a profile path");
        assert!(profile_dir.starts_with(QuotaConfig::profiles_dir()));
        assert_eq!(
            profile_dir.file_name().and_then(|name| name.to_str()),
            Some("codex-work-1")
        );

        fs::create_dir_all(QuotaConfig::profiles_dir()).expect("failed to create profile root");
        let outside = home.root.join("outside-profile");
        fs::create_dir_all(&outside).expect("failed to create outside profile dir");
        std::os::unix::fs::symlink(&outside, QuotaConfig::profiles_dir().join("codex-escape"))
            .expect("failed to create escaping profile symlink");

        let error = QuotaConfig::profile_dir(Provider::Codex, "escape")
            .expect_err("symlinked profile must not resolve outside profile root")
            .to_string();
        assert!(error.contains("resolves outside profile root"));
    }

    #[cfg(unix)]
    #[test]
    fn capture_writes_codex_isolated_home_owner_only() {
        let home = TempQuotaHome::new("quota-config-codex-capture");
        let codex_dir = home.home().join(".codex");
        fs::create_dir_all(&codex_dir).expect("failed to create codex auth dir");
        fs::write(codex_dir.join("auth.json"), br#"{"account":"active"}"#)
            .expect("failed to write codex auth");
        fs::write(codex_dir.join("installation_id"), b"active-installation\n")
            .expect("failed to write codex installation id");
        fs::write(codex_dir.join("config.toml"), b"model = \"gpt-5.6-sol\"\n")
            .expect("failed to write codex config");

        let profile_dir = home.profile_dir(Provider::Codex, "work");
        fs::create_dir_all(&profile_dir).expect("failed to create stale profile dir");
        fs::write(profile_dir.join("stale.json"), br#"{"account":"stale"}"#)
            .expect("failed to write stale profile file");

        copy_auth_to_profile(Provider::Codex, &profile_dir).expect("codex capture should succeed");

        let entries = fs::read_dir(&profile_dir)
            .expect("failed to read profile dir")
            .map(|entry| entry.expect("failed to read profile entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![OsString::from("codex-home")]);

        let codex_home = profile_dir.join("codex-home");
        let profile_auth = codex_home.join("auth.json");
        let meta = fs::symlink_metadata(&profile_auth).expect("failed to stat profile auth");
        assert!(meta.is_file());
        assert!(!meta.file_type().is_symlink());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);

        let config_meta =
            fs::symlink_metadata(codex_home.join("config.toml")).expect("failed to stat config");
        assert!(config_meta.is_file());
        assert_eq!(config_meta.permissions().mode() & 0o777, 0o600);

        assert!(
            !codex_home.join("installation_id").exists(),
            "capture must not copy the shared Codex installation id"
        );
    }

    #[cfg(unix)]
    #[test]
    fn prepare_codex_login_home_creates_profile_home_without_auth_or_installation() {
        let home = TempQuotaHome::new("quota-config-codex-login-home");
        let codex_dir = home.home().join(".codex");
        fs::create_dir_all(&codex_dir).expect("failed to create active codex dir");
        fs::write(codex_dir.join("config.toml"), b"model = \"gpt-5.6-sol\"\n")
            .expect("failed to write active codex config");
        fs::write(codex_dir.join("installation_id"), b"shared-installation\n")
            .expect("failed to write active codex installation id");

        let profile_dir = home.profile_dir(Provider::Codex, "work");
        let codex_home = prepare_codex_profile_login_home(&profile_dir)
            .expect("login home preparation should succeed");

        assert_eq!(codex_home, profile_dir.join("codex-home"));
        assert!(codex_home.is_dir());
        assert!(codex_home.join("config.toml").exists());
        assert!(!codex_home.join("auth.json").exists());
        assert!(!codex_home.join("installation_id").exists());
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
    fn save_is_atomic_owner_only_and_rejects_destination_symlink() {
        let home = TempConfigHome::new("quota-config-save-atomic");
        let config = QuotaConfig {
            accounts: vec![AccountEntry {
                name: "work-codex".into(),
                provider: Provider::Codex,
                live: false,
            }],
            selected_codex_account: Some("work-codex".into()),
            selected_claude_account: None,
        };
        let config_path = QuotaConfig::config_path();
        let config_dir = config_path
            .parent()
            .expect("config path should have parent");
        fs::create_dir_all(config_dir).expect("failed to create quota config dir");
        let outside = home.root.join("outside-config.toml");
        fs::write(&outside, "outside = true\n").expect("failed to seed outside config");
        std::os::unix::fs::symlink(&outside, &config_path)
            .expect("failed to symlink config destination");

        let error = config
            .save()
            .expect_err("symlinked config destination should be rejected")
            .to_string();
        assert!(error.contains("symlinked destination"));
        assert_eq!(
            fs::read_to_string(&outside).expect("failed to read outside config"),
            "outside = true\n"
        );
        assert!(
            fs::symlink_metadata(&config_path)
                .expect("failed to stat symlinked config")
                .file_type()
                .is_symlink(),
            "failed save should leave the symlink destination untouched"
        );
        fs::remove_file(&config_path).expect("failed to remove symlinked config");

        config.save().expect("config save should succeed");

        let saved = fs::read_to_string(&config_path).expect("failed to read saved config");
        assert!(saved.contains("name = \"work-codex\""));
        let mode = fs::metadata(&config_path)
            .expect("failed to stat saved config")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        let temp_files = fs::read_dir(config_dir)
            .expect("failed to read quota config dir")
            .map(|entry| {
                entry
                    .expect("failed to read quota config dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.starts_with(".config.toml.tmp-"))
            .collect::<Vec<_>>();
        assert!(
            temp_files.is_empty(),
            "unexpected config temp files: {temp_files:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_owner_only() {
        let _config_home = TempConfigHome::new("quota-config-save");
        let config = QuotaConfig {
            accounts: vec![AccountEntry {
                name: "work-codex".into(),
                provider: Provider::Codex,
                live: false,
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
