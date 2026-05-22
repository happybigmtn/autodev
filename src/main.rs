mod audit_command;
mod audit_everything;
mod backend_process;
mod book_command;
mod bug_command;
mod claude_exec;
mod cli;
mod codex_exec;
mod codex_stream;
mod completion_artifacts;
mod corpus;
mod design_command;
mod doctor_command;
mod generation;
mod health_command;
mod kimi_backend;
mod linear_tracker;
mod loop_command;
mod nemesis;
mod parallel_command;
mod pi_backend;
mod prompt_ethos;
mod qa_command;
mod qa_only_command;
mod quota_accounts;
mod quota_config;
mod quota_exec;
mod quota_patterns;
mod quota_selector;
mod quota_state;
mod quota_status;
mod quota_usage;
mod review_command;
mod ship_command;
mod spec_command;
mod state;
mod steward_command;
mod super_command;
mod symphony_command;
mod task_parser;
mod util;
mod verdict;
mod verification_lint;

use anyhow::Result;
use clap::Parser;

// The clap argument types live in `crate::cli`. Re-export them at the crate
// root so existing `crate::<ArgsType>` paths in command modules keep resolving.
pub(crate) use crate::cli::{
    AccountsCommand, AuditArgs, AuditEverythingPhase, AuditHarvestArgs, AuditResumeMode, BookArgs,
    BugArgs, Cli, Command, CorpusArgs, DesignArgs, GenerationArgs, HardeningProfile, HealthArgs,
    LoopArgs, NemesisArgs, ParallelAction, ParallelArgs, ParallelCargoTarget, QaArgs, QaOnlyArgs,
    QaTier, QuotaSubcommand, ReviewArgs, ShipArgs, SpecArgs, StewardArgs, SuperArgs, SymphonyArgs,
    SymphonyRunArgs, SymphonySubcommand, SymphonySyncArgs, SymphonyWorkflowArgs,
};

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Corpus(args) => generation::run_corpus(args).await,
        Command::Gen(args) => generation::run_gen(args).await,
        Command::Spec(args) => spec_command::run_spec(args).await,
        Command::Design(args) => design_command::run_design(args).await,
        Command::Super(args) => super_command::run_super(args).await,
        Command::Reverse(args) => generation::run_reverse(args).await,
        Command::Bug(args) => bug_command::run_bug(args).await,
        Command::Loop(args) => loop_command::run_loop(args).await,
        Command::Parallel(args) => parallel_command::run_parallel(args).await,
        Command::Qa(args) => qa_command::run_qa(args).await,
        Command::QaOnly(args) => qa_only_command::run_qa_only(args).await,
        Command::Health(args) => health_command::run_health(args).await,
        Command::Book(args) => book_command::run_book(args).await,
        Command::Doctor(args) => doctor_command::run_doctor(args).await,
        Command::Review(args) => review_command::run_review(args).await,
        Command::Steward(args) => steward_command::run_steward(args).await,
        Command::Audit(args) => audit_command::run_audit(args).await,
        Command::AuditHarvest(args) => super_command::run_audit_harvest_standalone(args).await,
        Command::Ship(args) => ship_command::run_ship(args).await,
        Command::Nemesis(args) => nemesis::run_nemesis(args).await,
        Command::Quota(args) => match args.command {
            QuotaSubcommand::Status => quota_status::run_status().await,
            QuotaSubcommand::Select(args) => {
                let provider: quota_config::Provider = args.provider.parse()?;
                quota_exec::run_quota_select(provider).await
            }
            QuotaSubcommand::Reset(args) => quota_status::run_reset(args.name.as_deref()),
            QuotaSubcommand::Open(args) => {
                let provider: quota_config::Provider = args.provider.parse()?;
                let code = quota_exec::run_quota_open(provider, &args.args).await?;
                std::process::exit(code);
            }
            QuotaSubcommand::Accounts(a) => match a.command {
                AccountsCommand::Add(args) => {
                    quota_accounts::run_accounts_add(&args.name, &args.provider)
                }
                AccountsCommand::List => quota_accounts::run_accounts_list(),
                AccountsCommand::Remove(args) => {
                    quota_accounts::run_accounts_remove(&args.name, args.force)
                }
                AccountsCommand::Capture(args) => quota_accounts::run_accounts_capture(&args.name),
            },
        },
        Command::Symphony(args) => symphony_command::run_symphony(args).await,
    }
}

#[cfg(test)]
mod tests {
    use crate::{Cli, Command, SymphonySubcommand};
    use clap::{CommandFactory, Parser};
    use std::path::Path;

    #[test]
    fn top_level_command_surface_matches_live_enum() {
        let expected = [
            "corpus",
            "gen",
            "spec",
            "design",
            "super",
            "reverse",
            "bug",
            "loop",
            "parallel",
            "qa",
            "qa-only",
            "health",
            "book",
            "doctor",
            "review",
            "steward",
            "audit",
            "ship",
            "nemesis",
            "quota",
            "audit-harvest",
            "symphony",
        ];
        let cli_command = Cli::command();
        let actual = cli_command
            .get_subcommands()
            .map(|command| command.get_name().to_string())
            .collect::<Vec<_>>();

        assert_eq!(actual, expected);

        for command in expected {
            let help = Cli::try_parse_from(["auto", command, "--help"]);
            assert!(help.is_err(), "expected help output for auto {command}");
        }
    }

    #[test]
    fn symphony_run_does_not_sync_by_default() {
        let cli = Cli::try_parse_from(["auto", "symphony", "run"]).expect("cli parse");
        let Command::Symphony(args) = cli.command else {
            panic!("expected symphony command");
        };
        let SymphonySubcommand::Run(args) = args.command else {
            panic!("expected symphony run");
        };
        assert!(!args.sync_first);
    }

    #[test]
    fn symphony_run_accepts_sync_first_flag() {
        let cli =
            Cli::try_parse_from(["auto", "symphony", "run", "--sync-first"]).expect("cli parse");
        let Command::Symphony(args) = cli.command else {
            panic!("expected symphony command");
        };
        let SymphonySubcommand::Run(args) = args.command else {
            panic!("expected symphony run");
        };
        assert!(args.sync_first);
    }

    #[test]
    fn review_includes_siblings_by_default() {
        let cli = Cli::try_parse_from(["auto", "review"]).expect("cli parse");
        let Command::Review(args) = cli.command else {
            panic!("expected review command");
        };
        assert!(args.include_siblings);
    }

    #[test]
    fn doctor_command_is_parseable() {
        let cli = Cli::try_parse_from(["auto", "doctor"]).expect("cli parse");
        let Command::Doctor(_) = cli.command else {
            panic!("expected doctor command");
        };

        let help = match Cli::try_parse_from(["auto", "doctor", "--help"]) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("expected help output"),
        };
        assert!(help.contains("Usage: auto doctor"));
    }

    #[test]
    fn design_command_is_parseable() {
        let cli = Cli::try_parse_from(["auto", "design", "sync UI to runtime"]).expect("cli parse");
        let Command::Design(args) = cli.command else {
            panic!("expected design command");
        };
        assert_eq!(args.prompt.as_deref(), Some("sync UI to runtime"));

        let help = match Cli::try_parse_from(["auto", "design", "--help"]) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("expected help output"),
        };
        assert!(help.contains("Usage: auto design"));
        assert!(help.contains("--skip-qa"));
    }

    #[test]
    fn super_command_is_parseable() {
        let cli = Cli::try_parse_from(["auto", "super", "make this repo production grade"])
            .expect("cli parse");
        let Command::Super(args) = cli.command else {
            panic!("expected super command");
        };
        assert_eq!(
            args.prompt.as_deref(),
            Some("make this repo production grade")
        );

        let help = match Cli::try_parse_from(["auto", "super", "--help"]) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("expected help output"),
        };
        assert!(help.contains("Usage: auto super"));
        assert!(help.contains("--resume"));
        assert!(help.contains("--skip-design"));
        assert!(help.contains("--design-resolve-passes"));

        let cli = Cli::try_parse_from(["auto", "super", "--resume", ".auto/super/run-1"])
            .expect("cli parse");
        let Command::Super(args) = cli.command else {
            panic!("expected super command");
        };
        assert_eq!(args.resume.as_deref(), Some(Path::new(".auto/super/run-1")));
    }

    #[test]
    fn bug_finder_and_skeptic_default_to_low_effort() {
        let cli = Cli::try_parse_from(["auto", "bug"]).expect("cli parse");
        let Command::Bug(args) = cli.command else {
            panic!("expected bug command");
        };

        assert_eq!(args.finder_effort, "low");
        assert_eq!(args.skeptic_effort, "low");
        assert_eq!(args.reviewer_effort, "high");
        assert_eq!(args.fixer_effort, "high");
        assert_eq!(args.finalizer_effort, "high");
    }

    #[test]
    fn symphony_run_help_mentions_symphony_root_env() {
        let help = match Cli::try_parse_from(["auto", "symphony", "run", "--help"]) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("expected help output"),
        };

        assert!(help.contains("--symphony-root <PATH>"));
        assert!(help.contains("AUTODEV_SYMPHONY_ROOT"));
    }
}
