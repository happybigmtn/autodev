use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[test]
fn pilot_dev_auto_finalizes_selected_task_before_closeout() {
    for command in ["bash", "git", "jq"] {
        if !command_available(command) {
            eprintln!("skipping pilot-dev wrapper test because `{command}` is unavailable");
            return;
        }
    }

    let root = unique_temp_dir("pilot-dev-finalize");
    let bin_dir = root.join("bin");
    let base_dir = root.join("repos");
    let repo_slug = "pilot-finalize-fixture";
    let repo = base_dir.join(repo_slug);
    let origin = root.join("origin.git");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    fs::create_dir_all(&base_dir).expect("base dir");

    run(
        Command::new("git").args(["init", "--bare"]).arg(&origin),
        &root,
    );
    fs::create_dir_all(&repo).expect("repo dir");
    run(Command::new("git").arg("init").arg(&repo), &root);
    run(
        Command::new("git").args(["-C"]).arg(&repo).args([
            "config",
            "user.email",
            "pilot-dev-test@example.invalid",
        ]),
        &root,
    );
    run(
        Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args(["config", "user.name", "Pilot Dev Test"]),
        &root,
    );
    fs::write(
        repo.join("IMPLEMENTATION_PLAN.md"),
        "\
# Implementation Plan

- [ ] `TASK-001` Prove automatic finalization
  Verification:
    - `test -f product.txt`
  Dependencies: none
",
    )
    .expect("plan");
    fs::write(
        repo.join("AGENTS.md"),
        "# Agent Instructions\n\nFixture repo.\n",
    )
    .expect("agents");
    fs::create_dir_all(repo.join("genesis")).expect("genesis");
    fs::write(repo.join("product.txt"), "initial\n").expect("product");
    run(
        Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args(["add", "."]),
        &root,
    );
    run(
        Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args(["commit", "-m", "fixture: initial plan"]),
        &root,
    );
    run(
        Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args(["branch", "-M", "main"]),
        &root,
    );
    run(
        Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args(["remote", "add", "origin"])
            .arg("git@github.com:example/pilot-finalize-fixture.git"),
        &root,
    );
    let rewrite_key = format!("url.file://{}.pushInsteadOf", origin.display());
    run(
        Command::new("git").args(["-C"]).arg(&repo).args([
            "config",
            rewrite_key.as_str(),
            "git@github.com:example/pilot-finalize-fixture.git",
        ]),
        &root,
    );
    run(
        Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args(["push", "-u", "origin", "main"]),
        &root,
    );

    write_executable(
        &bin_dir.join("auto"),
        &format!(
            "#!/usr/bin/env bash\nexec {:?} \"$@\"\n",
            env!("CARGO_BIN_EXE_auto")
        ),
    );
    write_executable(&bin_dir.join("codex"), &codex_stub_script());
    write_executable(
        &bin_dir.join("claude"),
        "#!/usr/bin/env bash\necho 'claude stub'\n",
    );
    write_executable(
        &bin_dir.join("gbrain"),
        "#!/usr/bin/env bash\nif [[ \"${1:-}\" == \"list\" ]]; then echo 'fixture-page'; exit 0; fi\necho 'gbrain stub'\n",
    );
    write_executable(
        &bin_dir.join("gh"),
        "#!/usr/bin/env bash\nif [[ \"${1:-}\" == \"auth\" ]]; then echo 'logged in'; exit 0; fi\nif [[ \"${1:-}\" == \"repo\" && \"${2:-}\" == \"view\" ]]; then echo '{\"viewerPermission\":\"WRITE\"}'; exit 0; fi\necho 'gh stub'\n",
    );
    write_executable(
        &bin_dir.join("hermes"),
        "#!/usr/bin/env bash\nif [[ \"${1:-}\" == \"--help\" ]]; then echo 'hermes stub'; exit 0; fi\ncat >/dev/null\n",
    );
    write_executable(
        &bin_dir.join("systemctl"),
        "#!/usr/bin/env bash\nif [[ \"${1:-}\" == \"--user\" && \"${2:-}\" == \"is-active\" ]]; then echo 'active'; exit 0; fi\necho 'systemctl stub'\n",
    );

    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/pilot-dev");
    let run_id = "pilot-dev-finalize-proof";
    let output = Command::new("bash")
        .arg(script)
        .arg(repo_slug)
        .arg("prove automatic task finalization")
        .env("PATH", path)
        .env("HOME", root.join("home"))
        .env("PILOT_BASE_DIR", &base_dir)
        .env("PILOT_RUN_ID", run_id)
        .env("PILOT_PLANNING_MODE", "none")
        .env("PILOT_REQUIRE_PLANNING_SPINE", "0")
        .env("PILOT_MIN_DISK_KB", "1")
        .env("PILOT_PROJECT_ROLLUP", "1")
        .env("PILOT_GBRAIN_PROJECT_ROLLUP", "0")
        .env("PILOT_AUTODEV_SOURCE", env!("CARGO_MANIFEST_DIR"))
        .env("GIT_AUTHOR_NAME", "Pilot Dev Test")
        .env("GIT_AUTHOR_EMAIL", "pilot-dev-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Pilot Dev Test")
        .env("GIT_COMMITTER_EMAIL", "pilot-dev-test@example.invalid")
        .output()
        .expect("pilot-dev runs");
    assert_success(&output, "pilot-dev");

    let run_root = repo.join(".auto/orchestrator").join(run_id);
    let plan = fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("plan");
    assert!(plan.contains("- [x] `TASK-001` Prove automatic finalization"));
    assert!(run_root.join("task-finalize.json").exists());
    assert!(run_root.join("pilot-closeout.json").exists());
    assert!(run_root.join("project-rollup.md").exists());
    assert!(run_root.join("phase-heartbeat.json").exists());
    assert!(run_root.join("phase-history.jsonl").exists());

    let closeout = fs::read_to_string(run_root.join("pilot-closeout.json")).expect("closeout");
    assert!(closeout.contains("\"status\": \"ok\""));
    let heartbeat: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(run_root.join("phase-heartbeat.json")).expect("heartbeat"),
    )
    .expect("heartbeat json");
    assert_eq!(heartbeat["phase"].as_str(), Some("finished"));
    assert_eq!(heartbeat["status"].as_str(), Some("ok"));
    let phase_history =
        fs::read_to_string(run_root.join("phase-history.jsonl")).expect("phase history");
    for expected in [
        "\"phase\": \"initialized\"",
        "\"phase\": \"worker-prompt\"",
        "\"phase\": \"codex-exec\"",
        "\"phase\": \"task-finalize\"",
        "\"phase\": \"closeout\"",
        "\"phase\": \"project-rollup\"",
        "\"phase\": \"finished\"",
    ] {
        assert!(
            phase_history.contains(expected),
            "missing phase history entry {expected}"
        );
    }
    let finalize = fs::read_to_string(run_root.join("task-finalize.json")).expect("finalize");
    assert!(finalize.contains("\"before_status\": \"pending\""));
    assert!(finalize.contains("\"after_status\": \"done\""));
    assert!(finalize.contains("\"pushed\": true"));
    let execution =
        fs::read_to_string(run_root.join("pilot-execution.json")).expect("execution manifest");
    let execution: serde_json::Value =
        serde_json::from_str(&execution).expect("execution manifest json");
    let task_finalize: serde_json::Value =
        serde_json::from_str(&finalize).expect("task finalize json");
    let implementation_commit = git_output(&repo, &["rev-parse", "HEAD~1"])
        .trim()
        .to_string();
    let finalize_commit = git_output(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    assert_eq!(
        execution["git"]["commit"].as_str(),
        Some(implementation_commit.as_str())
    );
    assert_eq!(
        execution["git"]["implementation_commit"].as_str(),
        Some(implementation_commit.as_str())
    );
    assert_eq!(
        execution["git"]["finalize_commit"].as_str(),
        Some(finalize_commit.as_str())
    );
    assert_eq!(
        task_finalize["git"]["implementation_commit"].as_str(),
        Some(implementation_commit.as_str())
    );
    assert_eq!(
        task_finalize["git"]["finalize_commit"].as_str(),
        Some(finalize_commit.as_str())
    );
    assert_ne!(implementation_commit, finalize_commit);

    let log = git_output(&repo, &["log", "--oneline", "-2"]);
    assert!(log.contains("pilot-finalize-fixture: finalize TASK-001 plan state"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("task_finalize: ok"));

    let _ = fs::remove_dir_all(root);
}

fn codex_stub_script() -> String {
    r##"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then
  echo "codex stub"
  exit 0
fi
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --output-last-message)
      out="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
prompt="$(cat)"
extract() {
  local label="$1"
  printf '%s\n' "$prompt" | awk -v label="$label" '$0 == label ":" { getline; print; exit }'
}
repo_slug="$(extract "Repository slug")"
workdir="$(extract "Workdir")"
run_root="$(extract "Run root")"
run_id="$(awk -F= '$1 == "run_id" { print $2; exit }' "$run_root/run.env")"
base_dir="$(dirname "$workdir")"
cd "$workdir"

printf 'implemented\n' >> product.txt
git add product.txt
git commit -m "fixture: implement TASK-001"
git push origin HEAD:main
commit="$(git rev-parse HEAD)"
branch="$(git branch --show-current)"

jq '
  def mark:
    .decision = (if .command == "auto pilot" or .command == "auto doctor" or .command == "auto command-surface" then "selected" else "deferred" end)
    | .reason = (if .command == "auto pilot" then "Selected for typed pilot proof." elif .command == "auto doctor" or .command == "auto command-surface" then "Selected by typed preflight." else "Deferred for focused wrapper finalization proof." end);
  .commands |= map(mark | .actions |= map(mark) | .subcommands |= map(mark | .subcommands |= map(mark)))
' "$run_root/autodev-command-selection.json" > "$run_root/autodev-command-selection.tmp"
mv "$run_root/autodev-command-selection.tmp" "$run_root/autodev-command-selection.json"
{
  echo "# Autodev Command Selection"
  echo
  echo "Run: \`$run_id\`"
  echo
  echo "| Surface | Decision | Reason and Evidence |"
  echo "|---|---|---|"
  jq -r '.commands[] | "| `" + .command + "` | " + .decision + " | " + .reason + " |"' "$run_root/autodev-command-selection.json"
} > "$run_root/autodev-command-selection.md"

cat > "$run_root/receipt.md" <<RECEIPT
# Receipt

## Inputs

- Repository: $repo_slug
- Task: TASK-001

## Commands

- test -f product.txt

## Artifacts

- $run_root/pilot-execution.json
- $run_root/task-finalize.json

## Tests

- test -f product.txt passed

## Commit

- $commit

## Risk

- Fixture-only wrapper proof; no production system touched.

## Next

- inspect project-rollup.md
RECEIPT

auto pilot "$repo_slug" "stub automatic finalization proof" \
  --base-dir "$base_dir" \
  --run-id "$run_id" \
  --run-root "$run_root" \
  --min-disk-kb 1 \
  --planning-mode none \
  --require-planning-spine false \
  --execution-update-only \
  --execution-status executed \
  --task-id TASK-001 \
  --task-title "Prove automatic finalization" \
  --task-source-plan IMPLEMENTATION_PLAN.md \
  --executor-kind codex \
  --executor-command "stub codex implemented TASK-001" \
  --executor-reason "integration test worker path" \
  --verification-command "test -f product.txt" \
  --verification-receipt "$run_root/receipt.md" \
  --verification-summary "fixture validation passed" \
  --git-branch "$branch" \
  --git-commit "$commit" \
  --git-pushed \
  --git-push-ref "origin/$branch" \
  --runtime-no-restart-reason "fixture file-only change" \
  --summary-branch-commit "$branch@${commit:0:7}" \
  --summary-plan "IMPLEMENTATION_PLAN.md" \
  --summary-design "not applicable" \
  --summary-execution "$run_root/pilot-execution.json" \
  --summary-tests "test -f product.txt" \
  --summary-closeout "$run_root/pilot-closeout.json" \
  --summary-next "inspect project-rollup.md" >/dev/null

if [[ -n "$out" ]]; then
  printf 'stub codex complete\n' > "$out"
fi
"##
    .to_string()
}

fn command_available(command: &str) -> bool {
    Command::new(command).arg("--version").output().is_ok()
}

fn write_executable(path: &Path, text: &str) {
    fs::write(path, text).expect("write executable");
    let mut perms = fs::metadata(path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("chmod");
}

fn unique_temp_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "autodev-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    fs::create_dir_all(&path).expect("temp dir");
    path
}

fn run(command: &mut Command, root: &Path) {
    let output = command
        .env("HOME", root.join("home"))
        .env("GIT_AUTHOR_NAME", "Pilot Dev Test")
        .env("GIT_AUTHOR_EMAIL", "pilot-dev-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Pilot Dev Test")
        .env("GIT_COMMITTER_EMAIL", "pilot-dev-test@example.invalid")
        .output()
        .expect("command runs");
    assert_success(&output, "setup command");
}

fn assert_success(output: &Output, label: &str) {
    if !output.status.success() {
        panic!(
            "{label} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn git_output(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git output");
    assert_success(&output, "git output");
    String::from_utf8_lossy(&output.stdout).to_string()
}
