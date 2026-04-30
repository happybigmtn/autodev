use std::fs;
use std::io::{self, Write};

use anyhow::Result;
use console::Style;

use crate::quota_config::{copy_auth_to_profile, AccountEntry, Provider, QuotaConfig};

pub(crate) fn run_accounts_add(name: &str, provider: &str) -> Result<()> {
    let provider: Provider = provider.parse()?;

    let mut config = QuotaConfig::load()?;

    let entry = AccountEntry {
        name: name.to_owned(),
        provider,
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
    eprintln!(
        "Capturing current {} credentials into profile '{name}'...",
        account.provider.label()
    );
    copy_auth_to_profile(account.provider, &profile_dir)?;
    eprintln!("Credentials updated for '{name}'.");
    Ok(())
}
