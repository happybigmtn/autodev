//! Narrow proof that a repository hook changed only queue-derived digest metadata.
//!
//! This optimization is deliberately separate from verification-receipt source
//! freshness. Receipts always use the exact source-state fingerprint. A
//! transition exists only for the single definition-of-done workspace probe
//! immediately following the host's own post-plan hook invocation.

use super::*;

use sha2::{Digest, Sha256};
use std::io::Read;
use std::process::Stdio;

use crate::completion_artifacts::{
    current_source_state_fingerprint_with_json_scalar_normalizations,
    SourceStateJsonScalarNormalization,
};

const CONFIG_PATH: &str = ".autodev-source-state.json";
const CONFIG_MAX_BYTES: u64 = 1024 * 1024;
const TARGET_MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RULES: usize = 8;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueDerivedSourceConfig {
    version: u32,
    #[serde(default)]
    queue_sha256: Vec<QueueDerivedSha256Rule>,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(deny_unknown_fields)]
struct QueueDerivedSha256Rule {
    target_path: String,
    target_pointer: String,
    source_path: String,
}

#[derive(Clone, Debug)]
pub(crate) struct QueueDerivedSourceBeforeHook {
    exact_source_state: String,
    normalized_source_state: String,
    rules: Vec<QueueDerivedSha256Rule>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct QueueDerivedSourceTransition {
    exact_before: String,
    exact_after: String,
}

impl QueueDerivedSourceTransition {
    pub(crate) fn matches_green_transition(&self, green: Option<&str>, current: &str) -> bool {
        green == Some(self.exact_before.as_str()) && current == self.exact_after
    }
}

#[derive(Clone, Copy)]
enum QueueSourceView {
    Head,
    Worktree,
}

pub(crate) fn capture_queue_derived_source_before_hook(
    repo_root: &Path,
) -> Result<Option<QueueDerivedSourceBeforeHook>> {
    let rules = load_rules(repo_root)?;
    if rules.is_empty() {
        return Ok(None);
    }
    validate_queue_derived_values(repo_root, &rules, QueueSourceView::Head)?;
    let exact_source_state = current_source_state_fingerprint(repo_root)?;
    let normalized_source_state = normalized_source_state(repo_root, &rules)?;
    Ok(Some(QueueDerivedSourceBeforeHook {
        exact_source_state,
        normalized_source_state,
        rules,
    }))
}

pub(crate) fn finish_queue_derived_source_after_hook(
    repo_root: &Path,
    before: Option<QueueDerivedSourceBeforeHook>,
    hook_reported_paths: &[String],
) -> Result<Option<QueueDerivedSourceTransition>> {
    let Some(before) = before else {
        return Ok(None);
    };
    for target in before
        .rules
        .iter()
        .map(|rule| rule.target_path.as_str())
        .collect::<BTreeSet<_>>()
    {
        if !hook_reported_paths.iter().any(|path| path == target) {
            bail!("post-plan hook did not report configured queue-derived target `{target}`");
        }
    }
    validate_queue_derived_values(repo_root, &before.rules, QueueSourceView::Worktree)?;
    let exact_after = current_source_state_fingerprint(repo_root)?;
    let normalized_after = normalized_source_state(repo_root, &before.rules)?;
    if before.normalized_source_state != normalized_after {
        return Ok(None);
    }
    Ok(Some(QueueDerivedSourceTransition {
        exact_before: before.exact_source_state,
        exact_after,
    }))
}

fn normalized_source_state(repo_root: &Path, rules: &[QueueDerivedSha256Rule]) -> Result<String> {
    let normalizations = rules
        .iter()
        .map(|rule| SourceStateJsonScalarNormalization {
            path: rule.target_path.clone(),
            pointer: rule.target_pointer.clone(),
        })
        .collect::<Vec<_>>();
    current_source_state_fingerprint_with_json_scalar_normalizations(repo_root, &normalizations)
}

fn load_rules(repo_root: &Path) -> Result<Vec<QueueDerivedSha256Rule>> {
    let path = repo_root.join(CONFIG_PATH);
    if !path.exists() {
        return Ok(Vec::new());
    }
    require_regular_tracked_file(repo_root, CONFIG_PATH, CONFIG_MAX_BYTES)?;
    let bytes = read_bounded_file(&path, CONFIG_MAX_BYTES, "queue-derived source config")?;
    let config = serde_json::from_slice::<QueueDerivedSourceConfig>(&bytes)
        .context("invalid queue-derived source config")?;
    if config.version != 1 {
        bail!(
            "unsupported queue-derived source config version {}; expected 1",
            config.version
        );
    }
    if config.queue_sha256.len() > MAX_RULES {
        bail!("queue-derived source config exceeds the {MAX_RULES} rule bound");
    }
    let mut canonical = config.queue_sha256.clone();
    canonical.sort();
    canonical.dedup();
    if canonical != config.queue_sha256 {
        bail!("queue-derived source rules must be sorted and unique");
    }
    let mut rules_per_target = BTreeMap::<&str, usize>::new();
    for rule in &canonical {
        let count = rules_per_target
            .entry(rule.target_path.as_str())
            .or_default();
        *count += 1;
        if *count > 16 {
            bail!(
                "queue-derived target `{}` exceeds the 16 pointer bound",
                rule.target_path
            );
        }
        if !safe_target_path(&rule.target_path) {
            bail!(
                "queue-derived target `{}` is not a safe source path",
                rule.target_path
            );
        }
        if !HOST_QUEUE_STATE_FILES.contains(&rule.source_path.as_str()) {
            bail!(
                "queue-derived source `{}` is not a host queue file",
                rule.source_path
            );
        }
        if !canonical_json_pointer(&rule.target_pointer) {
            bail!(
                "queue-derived pointer `{}` is not a canonical RFC 6901 pointer",
                rule.target_pointer
            );
        }
        require_regular_tracked_file(repo_root, &rule.target_path, TARGET_MAX_BYTES)?;
        require_regular_tracked_file(repo_root, &rule.source_path, TARGET_MAX_BYTES)?;
    }
    Ok(canonical)
}

fn validate_queue_derived_values(
    repo_root: &Path,
    rules: &[QueueDerivedSha256Rule],
    source_view: QueueSourceView,
) -> Result<()> {
    for rule in rules {
        // Revalidate immediately before every read. The post-plan hook runs
        // between capture and finish, so the pre-hook checks alone cannot
        // prevent it from replacing a tracked path's ancestor with a symlink.
        require_regular_tracked_file(repo_root, &rule.target_path, TARGET_MAX_BYTES)?;
        require_regular_tracked_file(repo_root, &rule.source_path, TARGET_MAX_BYTES)?;
        let target_bytes = read_bounded_file(
            &repo_root.join(&rule.target_path),
            TARGET_MAX_BYTES,
            &format!("queue-derived target `{}`", rule.target_path),
        )?;
        let target =
            serde_json::from_slice::<serde_json::Value>(&target_bytes).with_context(|| {
                format!(
                    "queue-derived target `{}` was not valid JSON",
                    rule.target_path
                )
            })?;
        let selected = target
            .pointer(&rule.target_pointer)
            .with_context(|| {
                format!(
                    "queue-derived pointer `{}` is absent in `{}`",
                    rule.target_pointer, rule.target_path
                )
            })?
            .as_str()
            .with_context(|| {
                format!(
                    "queue-derived pointer `{}` in `{}` must select a string",
                    rule.target_pointer, rule.target_path
                )
            })?;
        let expected = match source_view {
            QueueSourceView::Head => sha256_git_head_file(repo_root, &rule.source_path)?,
            QueueSourceView::Worktree => sha256_file_bounded(
                &repo_root.join(&rule.source_path),
                TARGET_MAX_BYTES,
                &format!("queue-derived source `{}`", rule.source_path),
            )?,
        };
        if selected != expected {
            let view = match source_view {
                QueueSourceView::Head => "HEAD",
                QueueSourceView::Worktree => "worktree",
            };
            bail!(
                "queue-derived pointer `{}` in `{}` does not equal SHA-256 of {view} `{}`",
                rule.target_pointer,
                rule.target_path,
                rule.source_path
            );
        }
    }
    Ok(())
}

fn safe_target_path(path: &str) -> bool {
    let relative = Path::new(path);
    !path.is_empty()
        && !path.chars().any(char::is_control)
        && !relative.is_absolute()
        && relative
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        && path != CONFIG_PATH
        && !HOST_QUEUE_STATE_FILES.contains(&path)
        && path != ".auto"
        && !path.starts_with(".auto/")
        && path != ".git"
        && !path.starts_with(".git/")
}

fn canonical_json_pointer(pointer: &str) -> bool {
    if pointer.is_empty() || !pointer.starts_with('/') || pointer.chars().any(char::is_control) {
        return false;
    }
    let bytes = pointer.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'~' {
            if index + 1 >= bytes.len() || !matches!(bytes[index + 1], b'0' | b'1') {
                return false;
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    true
}

fn require_regular_tracked_file(repo_root: &Path, path: &str, max_bytes: u64) -> Result<()> {
    let metadata = fs::symlink_metadata(repo_root.join(path))
        .with_context(|| format!("failed to inspect `{path}`"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > max_bytes {
        bail!("`{path}` must be a regular tracked file no larger than {max_bytes} bytes");
    }
    let canonical_root = fs::canonicalize(repo_root).context("failed to canonicalize repo root")?;
    let canonical_path = fs::canonicalize(repo_root.join(path))
        .with_context(|| format!("failed to canonicalize `{path}`"))?;
    if !canonical_path.starts_with(&canonical_root) {
        bail!("`{path}` resolves outside the repository");
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "--stage", "-z", "--", path])
        .output()
        .with_context(|| format!("failed checking tracked queue-derived path `{path}`"))?;
    if !output.status.success() {
        bail!("failed checking tracked queue-derived path `{path}`");
    }
    let records = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    let [record] = records.as_slice() else {
        bail!("`{path}` must be tracked exactly once at stage zero");
    };
    let record = std::str::from_utf8(record).context("git index record was not UTF-8")?;
    let (index, actual_path) = record
        .split_once('\t')
        .context("git index record lacked a path")?;
    let fields = index.split_whitespace().collect::<Vec<_>>();
    if actual_path != path || !matches!(fields.as_slice(), ["100644" | "100755", _, "0"]) {
        bail!("`{path}` must be a regular tracked file at stage zero");
    }
    Ok(())
}

fn read_bounded_file(path: &Path, max_bytes: u64, context: &str) -> Result<Vec<u8>> {
    let file = fs::File::open(path).with_context(|| format!("failed to open {context}"))?;
    let mut reader = file.take(max_bytes.saturating_add(1));
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {context}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        bail!("{context} exceeds the {max_bytes} byte bound");
    }
    Ok(bytes)
}

fn sha256_file_bounded(path: &Path, max_bytes: u64, context: &str) -> Result<String> {
    let file = fs::File::open(path).with_context(|| format!("failed to open {context}"))?;
    sha256_reader_bounded(file, max_bytes, context)
}

fn sha256_git_head_file(repo_root: &Path, path: &str) -> Result<String> {
    let spec = format!("HEAD:{path}");
    let size_output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["cat-file", "-s", spec.as_str()])
        .output()
        .with_context(|| format!("failed inspecting HEAD `{path}`"))?;
    if !size_output.status.success() {
        bail!("failed inspecting HEAD `{path}`");
    }
    let size = std::str::from_utf8(&size_output.stdout)
        .context("git cat-file size was not UTF-8")?
        .trim()
        .parse::<u64>()
        .context("git cat-file size was not an integer")?;
    if size > TARGET_MAX_BYTES {
        bail!("HEAD `{path}` exceeds the {TARGET_MAX_BYTES} byte bound");
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["cat-file", "blob", spec.as_str()])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed reading HEAD `{path}`"))?;
    let digest = child
        .stdout
        .take()
        .context("git cat-file stdout was unavailable")
        .and_then(|stdout| {
            sha256_reader_bounded(stdout, TARGET_MAX_BYTES, &format!("HEAD `{path}`"))
        });
    if digest.is_err() {
        let _ = child.kill();
    }
    let status = child
        .wait()
        .with_context(|| format!("failed waiting for HEAD `{path}`"))?;
    let digest = digest?;
    if !status.success() {
        bail!("failed reading HEAD `{path}`");
    }
    Ok(digest)
}

fn sha256_reader_bounded(reader: impl Read, max_bytes: u64, context: &str) -> Result<String> {
    let mut reader = reader.take(max_bytes.saturating_add(1));
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .with_context(|| format!("failed reading {context}"))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if total > max_bytes {
            bail!("{context} exceeds the {max_bytes} byte bound");
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nonce}"))
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git output UTF-8")
            .trim()
            .to_string()
    }

    #[test]
    fn queue_derived_reader_enforces_max_plus_one_bound() {
        let error = super::sha256_reader_bounded(
            std::io::Cursor::new(vec![0_u8; 10]),
            9,
            "bounded fixture",
        )
        .expect_err("max+1 input must fail closed");
        assert!(format!("{error:#}").contains("9 byte bound"));
    }

    #[test]
    fn tracked_target_cannot_escape_through_symlink_ancestor() {
        let root = temp_dir("queue-derived-symlink-root");
        let outside = temp_dir("queue-derived-symlink-outside");
        fs::create_dir_all(&root).expect("create repo");
        fs::create_dir_all(&outside).expect("create outside");
        git(&root, &["init", "-q"]);
        fs::write(outside.join("file.json"), "{}\n").expect("write outside target");
        let outside_path = outside.join("file.json");
        let object = git(
            &root,
            &[
                "hash-object",
                "-w",
                outside_path.to_str().expect("outside UTF-8"),
            ],
        );
        let cacheinfo = format!("100644,{object},escape/file.json");
        git(&root, &["update-index", "--add", "--cacheinfo", &cacheinfo]);
        symlink(&outside, root.join("escape")).expect("install escaping ancestor symlink");

        let error =
            super::require_regular_tracked_file(&root, "escape/file.json", super::TARGET_MAX_BYTES)
                .expect_err("canonical containment must reject symlink escape");
        assert!(format!("{error:#}").contains("outside the repository"));

        fs::remove_dir_all(&root).expect("remove repo");
        fs::remove_dir_all(&outside).expect("remove outside");
    }

    #[test]
    fn post_hook_target_cannot_escape_through_symlink_ancestor() {
        let root = temp_dir("queue-derived-post-hook-root");
        let outside = temp_dir("queue-derived-post-hook-outside");
        fs::create_dir_all(root.join("evidence")).expect("create repo target directory");
        fs::create_dir_all(&outside).expect("create outside");
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "tests@example.invalid"]);
        git(&root, &["config", "user.name", "Autodev Tests"]);
        fs::write(root.join("PLAN.md"), "# Plan\n").expect("write queue source");
        let digest = format!("{:x}", Sha256::digest(b"# Plan\n"));
        fs::write(
            root.join("evidence/manifest.json"),
            format!("{{\"plan_sha256\":\"{digest}\"}}\n"),
        )
        .expect("write target");
        fs::write(
            root.join(CONFIG_PATH),
            concat!(
                "{\"version\":1,\"queue_sha256\":[{",
                "\"target_path\":\"evidence/manifest.json\",",
                "\"target_pointer\":\"/plan_sha256\",",
                "\"source_path\":\"PLAN.md\"}]}\n"
            ),
        )
        .expect("write config");
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "fixture"]);

        let before = capture_queue_derived_source_before_hook(&root)
            .expect("capture before hook")
            .expect("configured transition");
        fs::write(
            outside.join("manifest.json"),
            format!("{{\"plan_sha256\":\"{digest}\"}}\n"),
        )
        .expect("write external target");
        fs::remove_dir_all(root.join("evidence")).expect("remove tracked target directory");
        symlink(&outside, root.join("evidence")).expect("install escaping ancestor symlink");

        let error = finish_queue_derived_source_after_hook(
            &root,
            Some(before),
            &["evidence/manifest.json".to_string()],
        )
        .expect_err("post-hook canonical containment must reject symlink escape");
        assert!(format!("{error:#}").contains("outside the repository"));

        fs::remove_file(root.join("evidence")).expect("remove symlink");
        fs::remove_dir_all(&root).expect("remove repo");
        fs::remove_dir_all(&outside).expect("remove outside");
    }
}
