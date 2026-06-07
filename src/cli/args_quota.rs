use clap::{Args, Subcommand};

#[derive(Args, Clone)]
pub(crate) struct QuotaArgs {
    #[command(subcommand)]
    pub(crate) command: QuotaSubcommand,
}

#[derive(Subcommand, Clone)]
pub(crate) enum QuotaSubcommand {
    /// Show quota status for all accounts
    Status,
    /// Select the primary account and activate its credentials for the provider
    Select(QuotaSelectArgs),
    /// Manage accounts
    Accounts(AccountsSubcommand),
    /// Force-clear exhausted status (all accounts, or one by name)
    Reset(QuotaResetArgs),
    /// Select the best account and launch the provider CLI
    Open(QuotaOpenArgs),
}

#[derive(Args, Clone)]
pub(crate) struct QuotaOpenArgs {
    /// Provider: "claude" or "codex"
    pub(crate) provider: String,
    /// Arguments passed through to the provider CLI
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(crate) args: Vec<String>,
}

#[derive(Args, Clone)]
pub(crate) struct QuotaSelectArgs {
    /// Provider: "claude" or "codex"
    pub(crate) provider: String,
}

#[derive(Args, Clone)]
pub(crate) struct QuotaResetArgs {
    /// Account name to reset. Omit to reset all.
    pub(crate) name: Option<String>,
}

#[derive(Args, Clone)]
pub(crate) struct AccountsSubcommand {
    #[command(subcommand)]
    pub(crate) command: AccountsCommand,
}

#[derive(Subcommand, Clone)]
pub(crate) enum AccountsCommand {
    /// Add a new account profile
    Add(AccountsAddArgs),
    /// List all configured accounts
    List,
    /// Log in to a Codex account inside this profile's isolated CODEX_HOME
    Login(AccountsLoginArgs),
    /// Remove an account profile
    Remove(AccountsRemoveArgs),
    /// Re-capture credentials from the current session into a profile
    Capture(AccountsCaptureArgs),
}

#[derive(Args, Clone)]
pub(crate) struct AccountsAddArgs {
    /// Account name (e.g., "work-codex-1")
    pub(crate) name: String,
    /// Provider: "claude" or "codex"
    pub(crate) provider: String,
}

#[derive(Args, Clone)]
pub(crate) struct AccountsRemoveArgs {
    /// Account name to remove
    pub(crate) name: String,
    /// Skip confirmation prompt
    #[arg(long)]
    pub(crate) force: bool,
}

#[derive(Args, Clone)]
pub(crate) struct AccountsCaptureArgs {
    /// Account name to update credentials for
    pub(crate) name: String,
}

#[derive(Args, Clone)]
pub(crate) struct AccountsLoginArgs {
    /// Account name to log in
    pub(crate) name: String,
    /// Codex binary to run
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: String,
    /// Arguments passed through to `codex login`
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(crate) args: Vec<String>,
}
