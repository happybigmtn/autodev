use std::fs;
use std::io::{self, Write};
use std::process::{Command, Stdio};

use anyhow::Result;
use console::Style;

use crate::quota_config::{
    copy_auth_to_profile, prepare_codex_profile_login_home, AccountEntry, Provider, QuotaConfig,
};

pub(crate) fn run_accounts_add(name: &str, provider: &str) -> Result<()> {
    let provider: Provider = provider.parse()?;

    let mut config = QuotaConfig::load()?;

    let entry = AccountEntry {
        name: name.to_owned(),
        provider,
        live: false,
    };

    config.add_account(entry)?;

    let profile_dir = QuotaConfig::profile_dir(provider, name)?;
    eprintln!(
        "Copying current {} credentials into profile '{name}'...",
        provider.label()
    );
    copy_auth_to_profile(provider, &profile_dir)?;

    config.save()?;
    eprintln!("Account '{name}' ({provider}) added.");
    Ok(())
}

pub(crate) fn run_accounts_add_live(name: &str) -> Result<()> {
    let provider = Provider::Codex;
    QuotaConfig::profile_dir(provider, name)?;
    let mut config = QuotaConfig::load()?;

    if config.find_account(name).is_some() {
        anyhow::bail!("account '{name}' already exists");
    }

    config.add_account(AccountEntry {
        name: name.to_owned(),
        provider,
        live: true,
    })?;
    config.save()?;
    eprintln!("Live Codex account '{name}' added.");
    Ok(())
}

pub(crate) fn run_accounts_list() -> Result<()> {
    let config = QuotaConfig::load()?;
    let bold = Style::new().bold();
    let dim = Style::new().dim();

    if config.accounts.is_empty() {
        eprintln!(
            "No accounts configured. Run `auto quota accounts add <name> <provider>` to get started."
        );
        return Ok(());
    }

    println!(
        "{:<20} {:<10} {}",
        bold.apply_to("NAME"),
        bold.apply_to("PROVIDER"),
        bold.apply_to("PROFILE"),
    );
    println!("{}", dim.apply_to("─".repeat(50)));

    for account in &config.accounts {
        let profile_dir = QuotaConfig::profile_dir(account.provider, &account.name)?;
        let exists = if profile_dir.exists() {
            "ok"
        } else {
            "MISSING"
        };
        println!(
            "{:<20} {:<10} {}",
            account.name,
            account.provider.label(),
            exists,
        );
    }

    Ok(())
}

pub(crate) fn run_accounts_remove(name: &str, force: bool) -> Result<()> {
    let mut config = QuotaConfig::load()?;

    if !force {
        eprint!("Remove account '{name}' and its credentials? [y/N] ");
        io::stderr().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            eprintln!("Cancelled.");
            return Ok(());
        }
    }

    let removed = config.remove_account(name)?;
    let profile_dir = QuotaConfig::profile_dir(removed.provider, name)?;
    if profile_dir.exists() {
        fs::remove_dir_all(&profile_dir)?;
    }
    config.save()?;
    eprintln!("Account '{name}' removed.");
    Ok(())
}

pub(crate) fn run_accounts_capture(name: &str) -> Result<()> {
    let config = QuotaConfig::load()?;
    let account = config
        .find_account(name)
        .ok_or_else(|| anyhow::anyhow!("account '{name}' not found"))?;

    let profile_dir = QuotaConfig::profile_dir(account.provider, name)?;
    if account.provider == Provider::Codex {
        eprintln!(
            "Note: captured Codex snapshots are transient; use `auto quota accounts add-live <name>` or `auto quota accounts login <name>` for durable credentials."
        );
    }
    eprintln!(
        "Capturing current {} credentials into profile '{name}'...",
        account.provider.label()
    );
    copy_auth_to_profile(account.provider, &profile_dir)?;
    eprintln!("Credentials updated for '{name}'.");
    Ok(())
}

pub(crate) fn run_accounts_login(name: &str, codex_bin: &str, args: &[String]) -> Result<()> {
    let config = QuotaConfig::load()?;
    let account = config
        .find_account(name)
        .ok_or_else(|| anyhow::anyhow!("account '{name}' not found"))?;

    if account.provider != Provider::Codex {
        anyhow::bail!(
            "`auto quota accounts login` currently supports codex accounts only; '{}' is {}",
            account.name,
            account.provider
        );
    }

    let profile_dir = QuotaConfig::profile_dir(account.provider, name)?;
    let codex_home = prepare_codex_profile_login_home(&profile_dir)?;
    eprintln!(
        "Launching codex login for profile '{name}' with CODEX_HOME={}...",
        codex_home.display()
    );

    let status = Command::new(codex_bin)
        .arg("login")
        .args(args)
        .env("CODEX_HOME", &codex_home)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| anyhow::anyhow!("failed to launch {codex_bin}: {error}"))?;

    if !status.success() {
        anyhow::bail!("codex login for profile '{name}' exited with {status}");
    }

    let auth_path = codex_home.join("auth.json");
    if !auth_path.exists() {
        anyhow::bail!(
            "codex login for profile '{name}' completed but did not write {}",
            auth_path.display()
        );
    }

    eprintln!("Codex credentials updated for '{name}'.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::path::PathBuf;
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
    #[test]
    fn add_live_adds_codex_live_account_and_rejects_duplicate() {
        let _home = TempConfigHome::new("quota-accounts-add-live");

        run_accounts_add_live("live").expect("add-live should save account");
        let config = QuotaConfig::load().expect("saved config should load");
        assert_eq!(config.accounts.len(), 1);
        assert_eq!(config.accounts[0].name, "live");
        assert_eq!(config.accounts[0].provider, Provider::Codex);
        assert!(config.accounts[0].live);
        assert_eq!(config.selected_codex_account.as_deref(), Some("live"));

        let error = run_accounts_add_live("live")
            .expect_err("duplicate add-live should fail")
            .to_string();
        assert!(error.contains("account 'live' already exists"));
    }
}
