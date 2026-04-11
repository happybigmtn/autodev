use anyhow::Result;
use console::Style;

use crate::quota_config::QuotaConfig;
use crate::quota_state::QuotaState;
use crate::quota_usage;

pub(crate) async fn run_status() -> Result<()> {
    let config = QuotaConfig::load()?;

    if config.accounts.is_empty() {
        eprintln!("No accounts configured. Run `auto quota accounts add <name> <provider>` to get started.");
        return Ok(());
    }

    let green = Style::new().green();
    let red = Style::new().red();
    let yellow = Style::new().yellow();
    let bold = Style::new().bold();
    let dim = Style::new().dim();

    for (i, account) in config.accounts.iter().enumerate() {
        if i > 0 {
            println!();
        }

        let profile_dir = QuotaConfig::profile_dir(account.provider, &account.name);
        let usage_result = quota_usage::fetch_usage(account.provider, &profile_dir).await;

        match usage_result {
            Ok(usage) => {
                let status = if usage.limit_reached {
                    red.apply_to("LIMIT HIT").to_string()
                } else if usage.weekly_used_pct >= 80 {
                    yellow.apply_to("low").to_string()
                } else {
                    green.apply_to("ok").to_string()
                };

                let primary_marker = if config.selected_account_name(account.provider)
                    == Some(account.name.as_str())
                {
                    " primary"
                } else {
                    ""
                };
                println!(
                    "  {} ({} {}{}) {}",
                    bold.apply_to(&account.name),
                    account.provider.label(),
                    usage.plan,
                    primary_marker,
                    status,
                );

                let session_remaining = 100u32.saturating_sub(usage.session_used_pct);
                let session_reset = format_secs(usage.session_resets_in_secs);
                print!("  session  ");
                print_bar(usage.session_used_pct, &green, &red, &yellow);
                println!(" {session_remaining:>3}% remaining  {session_reset}",);

                let weekly_remaining = 100u32.saturating_sub(usage.weekly_used_pct);
                let weekly_reset = format_secs(usage.weekly_resets_in_secs);
                print!("  weekly   ");
                print_bar(usage.weekly_used_pct, &green, &red, &yellow);
                println!(" {weekly_remaining:>3}% remaining  {weekly_reset}",);
            }
            Err(e) => {
                println!(
                    "  {} ({}) {}",
                    bold.apply_to(&account.name),
                    account.provider.label(),
                    red.apply_to(format!("error: {e:#}")),
                );
            }
        }
    }

    println!("{}", dim.apply_to(""));
    Ok(())
}

pub(crate) fn run_reset(name: Option<&str>) -> Result<()> {
    let mut state = QuotaState::load()?;
    match name {
        Some(name) => {
            state.reset_account(name);
            state.save()?;
            eprintln!("Account '{name}' reset to available.");
        }
        None => {
            state.reset_all();
            state.save()?;
            eprintln!("All accounts reset to available.");
        }
    }
    Ok(())
}

fn print_bar(used_pct: u32, green: &Style, red: &Style, yellow: &Style) {
    let total = 20;
    let filled = ((used_pct as usize) * total / 100).min(total);
    let empty = total - filled;

    let style = if used_pct >= 90 {
        red
    } else if used_pct >= 70 {
        yellow
    } else {
        green
    };

    let bar = format!("[{}{}]", "#".repeat(filled), "-".repeat(empty));
    print!("{}", style.apply_to(bar));
}

fn format_secs(secs: u64) -> String {
    if secs == 0 {
        return String::new();
    }
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    if hours > 0 {
        format!("resets in {hours}h{minutes:02}m")
    } else {
        format!("resets in {minutes}m")
    }
}
