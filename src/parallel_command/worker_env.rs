use super::*;

const WORKER_GIT_GUARD_DIR: &str = "worker-bin";
const WORKER_GIT_GUARD_BLOCKED_VERBS: [&str; 4] = ["push", "pull", "fetch", "rebase"];
const WORKER_GIT_GUARD_PROTOCOLS: [&str; 4] = ["ssh", "https", "http", "git"];
const CARGO_BUILD_JOBS_ENV: &str = "CARGO_BUILD_JOBS";
const CARGO_TARGET_DIR_ENV: &str = "CARGO_TARGET_DIR";
#[cfg(unix)]
static SECURED_DELETE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn parallel_run_root(repo_root: &Path, args: &ParallelArgs) -> PathBuf {
    match args.run_root.as_deref() {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => repo_root.join(path),
        None => crate::util::auto_run_root_override(repo_root, "parallel")
            .unwrap_or_else(|| repo_root.join(".auto").join("parallel")),
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct HostCargoEnv {
    vars: Vec<(String, String)>,
}

impl HostCargoEnv {
    #[cfg(test)]
    pub(crate) fn from_vars(vars: Vec<(String, String)>) -> Self {
        Self { vars }
    }

    fn set(&mut self, key: &str, value: &str) {
        upsert_env(&mut self.vars, key, value);
    }

    #[cfg(test)]
    pub(crate) fn value(&self, key: &str) -> Option<&str> {
        self.vars
            .iter()
            .rev()
            .find(|(existing, _)| existing == key)
            .map(|(_, value)| value.as_str())
    }

    #[cfg(test)]
    pub(crate) fn values(&self) -> impl Iterator<Item = &(String, String)> {
        self.vars.iter()
    }

    pub(crate) fn apply_to_tokio(&self, command: &mut tokio::process::Command) {
        command.envs(self.vars.iter().map(|(key, value)| (key, value)));
    }

    pub(crate) fn apply_to_std(&self, command: &mut Command) {
        command.envs(self.vars.iter().map(|(key, value)| (key, value)));
    }

    pub(crate) fn append_to(&self, env: &mut Vec<(String, String)>) {
        for (key, value) in &self.vars {
            upsert_env(env, key, value);
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LoopWorkerEnv {
    pub(crate) extra_env: Vec<(String, String)>,
    pub(crate) host_cargo_env: HostCargoEnv,
    run_owned_cargo_targets: Vec<PathBuf>,
    pub(crate) cargo_jobs_summary: String,
    pub(crate) cargo_target_summary: Option<String>,
    pub(crate) lane_local_cargo_target: bool,
    pub(crate) cargo_target_prompt_clause: String,
}

pub(crate) fn build_loop_worker_env(
    args: &ParallelArgs,
    repo_root: &Path,
    run_root: &Path,
) -> Result<LoopWorkerEnv> {
    let inherited = std::env::var("CARGO_BUILD_JOBS").ok();
    let inherited_target = std::env::var("CARGO_TARGET_DIR").ok();
    let parallelism = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(4);
    let mut worker_env = resolve_loop_worker_env(
        args.cargo_build_jobs,
        args.cargo_target,
        inherited.as_deref(),
        inherited_target.as_deref(),
        parallelism,
        args.max_concurrent_workers,
        repo_uses_cargo(repo_root),
        run_root,
    )?;
    prepare_run_owned_cargo_targets(&worker_env, run_root)?;
    install_parallel_worker_git_guard(&mut worker_env.extra_env, run_root)?;
    Ok(worker_env)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_loop_worker_env(
    cargo_build_jobs: Option<usize>,
    cargo_target: ParallelCargoTarget,
    inherited_cargo_build_jobs: Option<&str>,
    inherited_cargo_target_dir: Option<&str>,
    available_parallelism: usize,
    max_concurrent_workers: usize,
    repo_uses_cargo: bool,
    run_root: &Path,
) -> Result<LoopWorkerEnv> {
    if let Some(jobs) = cargo_build_jobs {
        if jobs == 0 {
            bail!("--cargo-build-jobs must be greater than 0");
        }
        return Ok(cargo_build_jobs_env(
            jobs,
            format!("override CARGO_BUILD_JOBS={jobs}"),
            cargo_target,
            inherited_cargo_target_dir,
            max_concurrent_workers,
            repo_uses_cargo,
            run_root,
        ));
    }

    if let Some(value) = inherited_cargo_build_jobs {
        let value = value.trim();
        if !value.is_empty() {
            let mut env = inherited_target_loop_worker_env(
                format!("inherited CARGO_BUILD_JOBS={value}"),
                cargo_target,
                inherited_cargo_target_dir,
                max_concurrent_workers,
                repo_uses_cargo,
                run_root,
            );
            upsert_env(&mut env.extra_env, CARGO_BUILD_JOBS_ENV, value);
            env.host_cargo_env.set(CARGO_BUILD_JOBS_ENV, value);
            return Ok(env);
        }
    }

    let jobs = default_cargo_build_jobs_for(available_parallelism, max_concurrent_workers);
    Ok(cargo_build_jobs_env(
        jobs,
        format!("auto CARGO_BUILD_JOBS={jobs}"),
        cargo_target,
        inherited_cargo_target_dir,
        max_concurrent_workers,
        repo_uses_cargo,
        run_root,
    ))
}

pub(crate) fn cargo_build_jobs_env(
    jobs: usize,
    cargo_jobs_summary: String,
    cargo_target: ParallelCargoTarget,
    inherited_cargo_target_dir: Option<&str>,
    max_concurrent_workers: usize,
    repo_uses_cargo: bool,
    run_root: &Path,
) -> LoopWorkerEnv {
    let mut env = inherited_target_loop_worker_env(
        cargo_jobs_summary,
        cargo_target,
        inherited_cargo_target_dir,
        max_concurrent_workers,
        repo_uses_cargo,
        run_root,
    );
    env.extra_env
        .push((CARGO_BUILD_JOBS_ENV.to_string(), jobs.to_string()));
    env.host_cargo_env
        .set(CARGO_BUILD_JOBS_ENV, &jobs.to_string());
    env
}

pub(crate) fn inherited_target_loop_worker_env(
    cargo_jobs_summary: String,
    cargo_target: ParallelCargoTarget,
    inherited_cargo_target_dir: Option<&str>,
    max_concurrent_workers: usize,
    repo_uses_cargo: bool,
    run_root: &Path,
) -> LoopWorkerEnv {
    let mut extra_env = Vec::new();
    let mut host_cargo_env = HostCargoEnv::default();
    let mut run_owned_cargo_targets = Vec::new();
    let cargo_target_layout = resolve_parallel_cargo_target_layout(
        cargo_target,
        inherited_cargo_target_dir,
        max_concurrent_workers,
        repo_uses_cargo,
        run_root,
    );
    let mut lane_local_cargo_target = false;
    let cargo_target_summary = match cargo_target_layout {
        ParallelCargoTargetLayout::None => None,
        ParallelCargoTargetLayout::Fixed(target_dir) => {
            extra_env.push((CARGO_TARGET_DIR_ENV.to_string(), target_dir.clone()));
            host_cargo_env.set(CARGO_TARGET_DIR_ENV, &target_dir);
            if cargo_target == ParallelCargoTarget::Shared
                || (cargo_target == ParallelCargoTarget::Lane && max_concurrent_workers <= 1)
            {
                run_owned_cargo_targets.push(PathBuf::from(&target_dir));
            }
            Some(target_dir)
        }
        ParallelCargoTargetLayout::LaneLocal => {
            lane_local_cargo_target = true;
            let host_target = run_root.join("host-cargo-target");
            host_cargo_env.set(CARGO_TARGET_DIR_ENV, &host_target.to_string_lossy());
            run_owned_cargo_targets.push(host_target);
            if lane_persistent_cargo_target_enabled() {
                Some(format!(
                    "lane-local persistent under {}/lane-caches/lane-*/cargo-target (survives per-task worktree churn)",
                    run_root.display()
                ))
            } else {
                Some(format!(
                    "lane-local under {}/lanes/lane-*/cargo-target",
                    run_root.display()
                ))
            }
        }
    };
    let cargo_target_prompt_clause =
        cargo_target_prompt_clause(lane_local_cargo_target, cargo_target_summary.as_deref());
    LoopWorkerEnv {
        extra_env,
        host_cargo_env,
        run_owned_cargo_targets,
        cargo_jobs_summary,
        cargo_target_summary,
        lane_local_cargo_target,
        cargo_target_prompt_clause,
    }
}

pub(crate) fn prepare_run_owned_cargo_targets(
    worker_env: &LoopWorkerEnv,
    run_root: &Path,
) -> Result<()> {
    for target in &worker_env.run_owned_cargo_targets {
        ensure_safe_private_cargo_target_dir(target, run_root)?;
    }
    Ok(())
}

#[cfg(unix)]
fn open_or_create_private_owned_dir(
    path: &Path,
    owned_root: &Path,
) -> Result<(
    std::os::fd::OwnedFd,
    std::os::fd::OwnedFd,
    std::ffi::OsString,
)> {
    use rustix::fs::{fchmod, fstat, mkdirat, open, openat, Mode, OFlags};
    use rustix::io::Errno;
    use rustix::process::geteuid;
    use std::ffi::OsString;
    use std::path::Component;

    const PRIVATE_DIR_MODE: u32 = 0o700;
    const GROUP_OR_OTHER_WRITE: u32 = 0o022;
    const STICKY: u32 = 0o1000;

    let safe_components = |candidate: &Path, label: &str| -> Result<Vec<OsString>> {
        let mut components = candidate.components();
        if !matches!(components.next(), Some(Component::RootDir)) {
            bail!("{label} must be an absolute path: {}", candidate.display());
        }
        components
            .map(|component| match component {
                Component::Normal(name) => Ok(name.to_os_string()),
                _ => bail!(
                    "{label} contains an unsafe path component: {}",
                    candidate.display()
                ),
            })
            .collect::<Result<Vec<OsString>>>()
    };
    let names = safe_components(path, "run-owned Cargo target")?;
    let owned_names = safe_components(owned_root, "Cargo-target owned root")?;
    if names.is_empty() {
        bail!("refusing to use the filesystem root as a Cargo target");
    }
    if owned_names.is_empty() || names.len() <= owned_names.len() {
        bail!(
            "run-owned Cargo target {} must be a descendant of owned root {}",
            path.display(),
            owned_root.display()
        );
    }
    if !names.starts_with(&owned_names) {
        bail!(
            "run-owned Cargo target {} escapes owned root {}",
            path.display(),
            owned_root.display()
        );
    }
    let owned_depth = owned_names.len();

    let open_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let create_mode = Mode::from_raw_mode(PRIVATE_DIR_MODE);
    let euid = geteuid().as_raw();
    let mut current = open("/", open_flags, Mode::empty())
        .context("failed to open filesystem root while securing Cargo target")?;
    let mut traversed = PathBuf::from("/");

    for (index, name) in names.iter().enumerate() {
        let parent_is_owned_root = index == owned_depth;
        let parent_is_run_owned_descendant = index > owned_depth;
        let parent_stat = fstat(&current).with_context(|| {
            format!(
                "failed to inspect Cargo-target ancestor {}",
                traversed.display()
            )
        })?;
        let parent_mode = parent_stat.st_mode & 0o7777;
        if parent_is_run_owned_descendant {
            if parent_stat.st_uid != euid || parent_mode & 0o777 != PRIVATE_DIR_MODE {
                bail!(
                    "run-owned Cargo-target ancestor {} must remain owned by uid {euid} with mode 0700 (found uid {}, mode {parent_mode:04o})",
                    traversed.display(),
                    parent_stat.st_uid
                );
            }
        } else if parent_is_owned_root {
            if parent_stat.st_uid != euid {
                bail!(
                    "Cargo-target owned root {} must be owned by current uid {euid} (found {})",
                    traversed.display(),
                    parent_stat.st_uid
                );
            }
            if parent_mode & GROUP_OR_OTHER_WRITE != 0 {
                bail!(
                    "Cargo-target owned root {} is group/other-writable (mode {parent_mode:04o}); choose a dedicated private run root or remove group/other write permission",
                    traversed.display()
                );
            }
        } else {
            let parent_is_root_owned_sticky = parent_stat.st_uid == 0 && parent_mode & STICKY != 0;
            if parent_stat.st_uid != 0 && parent_stat.st_uid != euid {
                bail!(
                    "Cargo-target ancestor {} is owned by untrusted uid {}",
                    traversed.display(),
                    parent_stat.st_uid
                );
            }
            if parent_mode & GROUP_OR_OTHER_WRITE != 0 && !parent_is_root_owned_sticky {
                bail!(
                    "Cargo-target ancestor {} is group/other-writable (mode {parent_mode:04o}); use a private AUTO_RUN_ROOT or remove group/other write permission",
                    traversed.display()
                );
            }
        }

        let child = match openat(&current, name, open_flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => {
                if index + 1 < owned_depth {
                    bail!(
                        "Cargo-target trusted ancestor {} does not exist",
                        traversed.join(name).display()
                    );
                }
                match mkdirat(&current, name, create_mode) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(err) => {
                        return Err(err).with_context(|| {
                            format!(
                                "failed to create private Cargo-target directory {}",
                                traversed.join(name).display()
                            )
                        })
                    }
                }
                openat(&current, name, open_flags, Mode::empty()).with_context(|| {
                    format!(
                        "failed to open newly created Cargo-target directory {} without following symlinks",
                        traversed.join(name).display()
                    )
                })?
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "refusing Cargo-target component {} that is not a no-follow directory (possible symlink)",
                        traversed.join(name).display()
                    )
                })
            }
        };
        traversed.push(name);
        let child_stat = fstat(&child).with_context(|| {
            format!(
                "failed to inspect opened Cargo-target component {}",
                traversed.display()
            )
        })?;
        let child_is_owned_root = index + 1 == owned_depth;
        let child_is_run_owned_descendant = index + 1 > owned_depth;
        if child_is_run_owned_descendant {
            if child_stat.st_uid != euid {
                bail!(
                    "run-owned Cargo-target component {} must be owned by current uid {euid} (found {})",
                    traversed.display(),
                    child_stat.st_uid
                );
            }
            fchmod(&child, create_mode).with_context(|| {
                format!(
                    "failed to make opened run-owned Cargo-target component private via descriptor: {}",
                    traversed.display()
                )
            })?;
            let repaired_stat = fstat(&child).with_context(|| {
                format!(
                    "failed to re-inspect opened Cargo-target component {}",
                    traversed.display()
                )
            })?;
            let repaired_mode = repaired_stat.st_mode & 0o777;
            if repaired_stat.st_uid != euid || repaired_mode != PRIVATE_DIR_MODE {
                bail!(
                    "run-owned Cargo-target component {} must remain owned by uid {euid} with mode 0700 (found uid {}, mode {repaired_mode:04o})",
                    traversed.display(),
                    repaired_stat.st_uid
                );
            }
        } else if child_is_owned_root {
            let child_mode = child_stat.st_mode & 0o7777;
            if child_stat.st_uid != euid {
                bail!(
                    "Cargo-target owned root {} must be owned by current uid {euid} (found {})",
                    traversed.display(),
                    child_stat.st_uid
                );
            }
            if child_mode & GROUP_OR_OTHER_WRITE != 0 {
                bail!(
                    "Cargo-target owned root {} is group/other-writable (mode {child_mode:04o}); choose a dedicated private run root or remove group/other write permission",
                    traversed.display()
                );
            }
        } else {
            let child_mode = child_stat.st_mode & 0o7777;
            if child_stat.st_uid != 0 && child_stat.st_uid != euid {
                bail!(
                    "Cargo-target ancestor {} is owned by untrusted uid {}",
                    traversed.display(),
                    child_stat.st_uid
                );
            }
            let child_is_root_owned_sticky = child_stat.st_uid == 0 && child_mode & STICKY != 0;
            if child_mode & GROUP_OR_OTHER_WRITE != 0 && !child_is_root_owned_sticky {
                bail!(
                    "Cargo-target ancestor {} is group/other-writable (mode {child_mode:04o}); use a private AUTO_RUN_ROOT or remove group/other write permission",
                    traversed.display()
                );
            }
        }
        if index + 1 == names.len() {
            return Ok((current, child, name.clone()));
        }
        current = child;
    }

    unreachable!("an absolute non-root target path has at least one component")
}

#[cfg(all(unix, test))]
pub(crate) fn ensure_private_cargo_target_dir(path: &Path, owned_root: &Path) -> Result<()> {
    let _ = open_or_create_private_owned_dir(path, owned_root)?;
    Ok(())
}

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct SecuredCargoTargetDir {
    path: PathBuf,
    parent: std::os::fd::OwnedFd,
    directory: std::os::fd::OwnedFd,
    name: std::ffi::OsString,
    parent_dev: u64,
    parent_ino: u64,
    target_dev: u64,
    target_ino: u64,
}

#[cfg(unix)]
#[derive(Debug)]
struct SecuredDirectoryEntry {
    name: std::ffi::OsString,
    dev: u64,
    ino: u64,
    mode: rustix::fs::RawMode,
    size: u64,
}

#[cfg(unix)]
fn open_absolute_dir_no_follow(
    path: &Path,
) -> Result<(
    std::os::fd::OwnedFd,
    std::os::fd::OwnedFd,
    std::ffi::OsString,
)> {
    use rustix::fs::{open, openat, Mode, OFlags};
    use rustix::io::dup;
    use std::path::Component;

    let mut components = path.components();
    if !matches!(components.next(), Some(Component::RootDir)) {
        bail!(
            "secured Cargo-target path must be absolute: {}",
            path.display()
        );
    }
    let names = components
        .map(|component| match component {
            Component::Normal(name) => Ok(name.to_os_string()),
            _ => bail!(
                "secured Cargo-target path contains an unsafe component: {}",
                path.display()
            ),
        })
        .collect::<Result<Vec<_>>>()?;
    if names.is_empty() {
        bail!("refusing to secure the filesystem root as a Cargo target");
    }

    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut current = open("/", flags, Mode::empty())
        .context("failed to open filesystem root while anchoring Cargo target")?;
    for (index, name) in names.iter().enumerate() {
        let parent = if index + 1 == names.len() {
            Some(
                dup(&current)
                    .context("failed to retain Cargo-target parent directory descriptor")?,
            )
        } else {
            None
        };
        let child = openat(&current, name, flags, Mode::empty()).with_context(|| {
            format!(
                "refusing Cargo-target component {} that is not a no-follow directory (possible symlink)",
                Path::new("/").join(names[..=index].iter().collect::<PathBuf>())
                    .display()
            )
        })?;
        if let Some(parent) = parent {
            return Ok((parent, child, name.clone()));
        }
        current = child;
    }
    unreachable!("an absolute non-root path has at least one component")
}

#[cfg(unix)]
pub(crate) fn secure_private_cargo_target_dir(
    path: &Path,
    owned_root: &Path,
) -> Result<SecuredCargoTargetDir> {
    secure_private_cargo_target_dir_with_hook(path, owned_root, || Ok(()))
}

#[cfg(unix)]
pub(crate) fn secure_private_cargo_target_dir_with_hook(
    path: &Path,
    owned_root: &Path,
    after_validated_open: impl FnOnce() -> Result<()>,
) -> Result<SecuredCargoTargetDir> {
    use rustix::fs::{fstat, FileType};

    // Return the exact descriptors opened by the validating traversal. Never
    // validate, drop, and reopen by path: a rename-exchange in that gap could
    // otherwise substitute an unrelated same-uid directory.
    let (parent, directory, name) = open_or_create_private_owned_dir(path, owned_root)?;
    after_validated_open()?;
    let parent_stat = fstat(&parent).with_context(|| {
        format!(
            "failed to inspect secured Cargo-target parent for {}",
            path.display()
        )
    })?;
    let target_stat = fstat(&directory)
        .with_context(|| format!("failed to inspect secured Cargo target {}", path.display()))?;
    if !FileType::from_raw_mode(target_stat.st_mode).is_dir() {
        bail!(
            "secured Cargo target is not a directory: {}",
            path.display()
        );
    }
    let secured = SecuredCargoTargetDir {
        path: path.to_path_buf(),
        parent,
        directory,
        name,
        parent_dev: parent_stat.st_dev,
        parent_ino: parent_stat.st_ino,
        target_dev: target_stat.st_dev,
        target_ino: target_stat.st_ino,
    };
    secured.revalidate_path()?;
    Ok(secured)
}

#[cfg(unix)]
pub(crate) fn ensure_safe_private_cargo_target_dir(path: &Path, owned_root: &Path) -> Result<()> {
    secure_private_cargo_target_dir(path, owned_root)?.validate_safe_tree()
}

#[cfg(unix)]
impl SecuredCargoTargetDir {
    fn same_identity(stat: &rustix::fs::Stat, expected_dev: u64, expected_ino: u64) -> bool {
        stat.st_dev == expected_dev && stat.st_ino == expected_ino
    }

    fn revalidate_path(&self) -> Result<()> {
        use rustix::fs::{fstat, statat, AtFlags, FileType};

        let held_parent = fstat(&self.parent).with_context(|| {
            format!(
                "failed to re-inspect held Cargo-target parent for {}",
                self.path.display()
            )
        })?;
        if !Self::same_identity(&held_parent, self.parent_dev, self.parent_ino) {
            bail!(
                "held Cargo-target parent identity changed for {}",
                self.path.display()
            );
        }
        let held_target = fstat(&self.directory).with_context(|| {
            format!(
                "failed to re-inspect held Cargo-target descriptor {}",
                self.path.display()
            )
        })?;
        if !FileType::from_raw_mode(held_target.st_mode).is_dir()
            || !Self::same_identity(&held_target, self.target_dev, self.target_ino)
        {
            bail!(
                "held Cargo-target identity changed for {}",
                self.path.display()
            );
        }
        let linked_target = statat(&self.parent, &self.name, AtFlags::SYMLINK_NOFOLLOW)
            .with_context(|| {
                format!(
                    "Cargo-target path no longer names the secured directory {}",
                    self.path.display()
                )
            })?;
        if !FileType::from_raw_mode(linked_target.st_mode).is_dir()
            || !Self::same_identity(&linked_target, self.target_dev, self.target_ino)
        {
            bail!(
                "Cargo-target path identity changed after secure open: {}",
                self.path.display()
            );
        }

        // Reopen the absolute no-follow chain as well. This detects an
        // ancestor rename/replacement that an anchored parent descriptor alone
        // cannot observe.
        let (reopened_parent, reopened_target, _) = open_absolute_dir_no_follow(&self.path)?;
        let reopened_parent_stat = fstat(&reopened_parent).with_context(|| {
            format!(
                "failed to inspect reopened Cargo-target parent for {}",
                self.path.display()
            )
        })?;
        let reopened_target_stat = fstat(&reopened_target).with_context(|| {
            format!(
                "failed to inspect reopened Cargo target {}",
                self.path.display()
            )
        })?;
        if !Self::same_identity(&reopened_parent_stat, self.parent_dev, self.parent_ino)
            || !Self::same_identity(&reopened_target_stat, self.target_dev, self.target_ino)
        {
            bail!(
                "Cargo-target absolute path was replaced after secure open: {}",
                self.path.display()
            );
        }
        Ok(())
    }

    fn entries_for(directory: &std::os::fd::OwnedFd) -> Result<Vec<SecuredDirectoryEntry>> {
        use rustix::fs::{openat, statat, AtFlags, Mode, OFlags, RawDir};
        use std::mem::MaybeUninit;
        use std::os::unix::ffi::OsStrExt;

        let scan_fd = openat(
            directory,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .context("failed to duplicate secured directory for descriptor-relative scan")?;
        let mut buffer = [MaybeUninit::<u8>::uninit(); 8192];
        let mut scan = RawDir::new(scan_fd, &mut buffer);
        let mut entries = Vec::new();
        while let Some(entry) = scan.next() {
            let entry = entry.context("failed to read secured Cargo-target directory")?;
            let name_bytes = entry.file_name().to_bytes();
            if name_bytes == b"." || name_bytes == b".." {
                continue;
            }
            let name = std::ffi::OsStr::from_bytes(name_bytes).to_os_string();
            let stat = statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)
                .context("failed to inspect secured Cargo-target entry without following links")?;
            entries.push(SecuredDirectoryEntry {
                name,
                dev: stat.st_dev,
                ino: stat.st_ino,
                mode: stat.st_mode,
                size: stat.st_size.max(0) as u64,
            });
        }
        Ok(entries)
    }

    fn entry_named(
        directory: &std::os::fd::OwnedFd,
        name: &std::ffi::OsStr,
    ) -> Result<Option<SecuredDirectoryEntry>> {
        use rustix::fs::{statat, AtFlags};
        use rustix::io::Errno;

        match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(Some(SecuredDirectoryEntry {
                name: name.to_os_string(),
                dev: stat.st_dev,
                ino: stat.st_ino,
                mode: stat.st_mode,
                size: stat.st_size.max(0) as u64,
            })),
            Err(Errno::NOENT) => Ok(None),
            Err(err) => Err(err).context("failed to inspect secured directory entry"),
        }
    }

    fn revalidate_entry(
        parent: &std::os::fd::OwnedFd,
        entry: &SecuredDirectoryEntry,
    ) -> Result<rustix::fs::Stat> {
        use rustix::fs::{statat, AtFlags, FileType};
        use rustix::process::geteuid;

        let current = statat(parent, &entry.name, AtFlags::SYMLINK_NOFOLLOW)
            .context("secured Cargo-target entry changed or disappeared")?;
        if !Self::same_identity(&current, entry.dev, entry.ino)
            || FileType::from_raw_mode(current.st_mode) != FileType::from_raw_mode(entry.mode)
        {
            bail!(
                "secured Cargo-target entry identity changed during traversal: {}",
                entry.name.to_string_lossy()
            );
        }
        if current.st_uid != geteuid().as_raw() {
            bail!(
                "refusing to inspect or delete Cargo-target entry {} owned by uid {}",
                entry.name.to_string_lossy(),
                current.st_uid
            );
        }
        Ok(current)
    }

    fn open_child_dir(
        parent: &std::os::fd::OwnedFd,
        entry: &SecuredDirectoryEntry,
    ) -> Result<std::os::fd::OwnedFd> {
        use rustix::fs::{fstat, openat, FileType, Mode, OFlags};

        Self::revalidate_entry(parent, entry)?;
        let child = openat(
            parent,
            &entry.name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .with_context(|| {
            format!(
                "refusing Cargo-target child {} that is not a no-follow directory",
                entry.name.to_string_lossy()
            )
        })?;
        let opened = fstat(&child).context("failed to inspect opened Cargo-target child")?;
        if !FileType::from_raw_mode(opened.st_mode).is_dir()
            || !Self::same_identity(&opened, entry.dev, entry.ino)
        {
            bail!(
                "Cargo-target child identity changed while opening: {}",
                entry.name.to_string_lossy()
            );
        }
        Ok(child)
    }

    fn size_recursive(directory: &std::os::fd::OwnedFd) -> Result<u64> {
        use rustix::fs::FileType;

        let mut total = 0u64;
        for entry in Self::entries_for(directory)? {
            Self::revalidate_entry(directory, &entry)?;
            if FileType::from_raw_mode(entry.mode).is_dir() {
                let child = Self::open_child_dir(directory, &entry)?;
                total = total.saturating_add(Self::size_recursive(&child)?);
                Self::revalidate_entry(directory, &entry)?;
            } else {
                // Symlinks and special files contribute only their own inode
                // size. They are never opened or followed.
                total = total.saturating_add(entry.size);
            }
        }
        Ok(total)
    }

    fn validate_safe_tree_recursive(directory: &std::os::fd::OwnedFd) -> Result<()> {
        use rustix::fs::{fchmod, fstat, FileType, Mode};

        let parent =
            fstat(directory).context("failed to inspect secured Cargo-target directory")?;
        for entry in Self::entries_for(directory)? {
            let current = Self::revalidate_entry(directory, &entry)?;
            let file_type = FileType::from_raw_mode(entry.mode);
            if file_type.is_symlink() {
                bail!(
                    "refusing internal symlink in run-owned Cargo target: {}",
                    entry.name.to_string_lossy()
                );
            }
            if file_type.is_dir() {
                if current.st_dev != parent.st_dev {
                    bail!(
                        "refusing mounted/cross-device directory in run-owned Cargo target: {}",
                        entry.name.to_string_lossy()
                    );
                }
                let child = Self::open_child_dir(directory, &entry)?;
                fchmod(&child, Mode::from_raw_mode(0o700)).with_context(|| {
                    format!(
                        "failed to make run-owned Cargo-target directory private: {}",
                        entry.name.to_string_lossy()
                    )
                })?;
                Self::validate_safe_tree_recursive(&child)?;
                Self::revalidate_entry(directory, &entry)?;
            } else if !file_type.is_file() {
                bail!(
                    "refusing unsafe special file in run-owned Cargo target: {}",
                    entry.name.to_string_lossy()
                );
            }
        }
        Ok(())
    }

    pub(crate) fn validate_safe_tree(&self) -> Result<()> {
        self.revalidate_path()?;
        Self::validate_safe_tree_recursive(&self.directory)?;
        self.revalidate_path()
    }

    pub(crate) fn reset_managed_child_directory(&self, name: &std::ffi::OsStr) -> Result<()> {
        self.reset_managed_child_directory_with_hook(name, || Ok(()))
    }

    fn reset_managed_child_directory_with_hook(
        &self,
        name: &std::ffi::OsStr,
        after_child_open: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        use rustix::fs::{fchmod, mkdirat, Mode};

        if name.is_empty() || name == "." || name == ".." {
            bail!("managed child directory name is unsafe");
        }
        self.revalidate_path()?;
        let (child, entry, should_clear) = match Self::entry_named(&self.directory, name)? {
            Some(entry) => {
                let child = Self::open_child_dir(&self.directory, &entry)?;
                fchmod(&child, Mode::from_raw_mode(0o700)).with_context(|| {
                    format!(
                        "failed to make managed child directory private: {}",
                        name.to_string_lossy()
                    )
                })?;
                (child, entry, true)
            }
            None => {
                mkdirat(&self.directory, name, Mode::from_raw_mode(0o700)).with_context(|| {
                    format!(
                        "failed to create managed child directory {}",
                        name.to_string_lossy()
                    )
                })?;
                let entry = Self::entry_named(&self.directory, name)?
                    .context("new managed child directory disappeared")?;
                (Self::open_child_dir(&self.directory, &entry)?, entry, false)
            }
        };
        after_child_open()?;
        Self::revalidate_entry(&self.directory, &entry)?;
        if should_clear {
            Self::clear_recursive(&child)?;
            Self::revalidate_entry(&self.directory, &entry)?;
        }
        let child_stat =
            rustix::fs::fstat(&child).context("failed to inspect reset managed child directory")?;
        if !rustix::fs::FileType::from_raw_mode(child_stat.st_mode).is_dir() {
            bail!("reset managed child is no longer a directory");
        }
        Self::revalidate_entry(&self.directory, &entry)?;
        self.revalidate_path()
    }

    fn quarantine_entry_with_hook(
        directory: &std::os::fd::OwnedFd,
        entry: &SecuredDirectoryEntry,
        before_quarantine: &mut impl FnMut(&std::os::fd::OwnedFd, &SecuredDirectoryEntry) -> Result<()>,
    ) -> Result<SecuredDirectoryEntry> {
        use rustix::fs::{renameat_with, statat, AtFlags, RenameFlags};
        use rustix::io::Errno;
        use std::sync::atomic::Ordering;

        Self::revalidate_entry(directory, entry)?;
        before_quarantine(directory, entry)?;
        for _ in 0..100 {
            let sequence = SECURED_DELETE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let quarantine_name = std::ffi::OsString::from(format!(
                ".auto-secured-delete-{}-{}-{sequence}",
                timestamp_slug(),
                std::process::id(),
            ));
            match renameat_with(
                directory,
                &entry.name,
                directory,
                &quarantine_name,
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) => {
                    let stat = statat(directory, &quarantine_name, AtFlags::SYMLINK_NOFOLLOW)
                        .context("quarantined secured entry disappeared")?;
                    let quarantined = SecuredDirectoryEntry {
                        name: quarantine_name.clone(),
                        dev: stat.st_dev,
                        ino: stat.st_ino,
                        mode: stat.st_mode,
                        size: stat.st_size.max(0) as u64,
                    };
                    if !Self::same_identity(&stat, entry.dev, entry.ino)
                        || rustix::fs::FileType::from_raw_mode(stat.st_mode)
                            != rustix::fs::FileType::from_raw_mode(entry.mode)
                    {
                        let restore = renameat_with(
                            directory,
                            &quarantine_name,
                            directory,
                            &entry.name,
                            RenameFlags::NOREPLACE,
                        );
                        return match restore {
                            Ok(()) => Err(anyhow::anyhow!(
                                "secured entry identity changed before quarantine: {}",
                                entry.name.to_string_lossy()
                            )),
                            Err(restore_err) => Err(anyhow::anyhow!(
                                "secured entry identity changed before quarantine: {}; failed restoring unexpected entry: {restore_err}",
                                entry.name.to_string_lossy()
                            )),
                        };
                    }
                    return Ok(quarantined);
                }
                Err(Errno::EXIST) => continue,
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!(
                            "failed to quarantine secured entry {} before deletion",
                            entry.name.to_string_lossy()
                        )
                    })
                }
            }
        }
        bail!(
            "failed reserving quarantine name for secured entry {}",
            entry.name.to_string_lossy()
        )
    }

    fn remove_entry_recursive_with_hook(
        directory: &std::os::fd::OwnedFd,
        entry: &SecuredDirectoryEntry,
        before_quarantine: &mut impl FnMut(&std::os::fd::OwnedFd, &SecuredDirectoryEntry) -> Result<()>,
    ) -> Result<u64> {
        use rustix::fs::{unlinkat, AtFlags, FileType};

        let quarantined = Self::quarantine_entry_with_hook(directory, entry, before_quarantine)?;
        if FileType::from_raw_mode(quarantined.mode).is_dir() {
            use rustix::fs::{fchmod, Mode};
            let child = Self::open_child_dir(directory, &quarantined)?;
            fchmod(&child, Mode::from_raw_mode(0o700)).with_context(|| {
                format!(
                    "failed to make quarantined secured directory private: {}",
                    quarantined.name.to_string_lossy()
                )
            })?;
            let freed = Self::size_recursive(&child)?;
            Self::clear_recursive_with_hook(&child, before_quarantine)?;
            Self::revalidate_entry(directory, &quarantined)?;
            before_quarantine(directory, &quarantined)?;
            Self::revalidate_entry(directory, &quarantined)?;
            unlinkat(directory, &quarantined.name, AtFlags::REMOVEDIR).with_context(|| {
                format!(
                    "failed to remove quarantined secured directory {}",
                    quarantined.name.to_string_lossy()
                )
            })?;
            Ok(freed)
        } else {
            // The link/special/file inode is first moved to a unique
            // quarantine name and identity-checked. A swap at the original
            // name therefore fails before unlink can target the replacement.
            Self::revalidate_entry(directory, &quarantined)?;
            before_quarantine(directory, &quarantined)?;
            Self::revalidate_entry(directory, &quarantined)?;
            unlinkat(directory, &quarantined.name, AtFlags::empty()).with_context(|| {
                format!(
                    "failed to remove quarantined secured entry {}",
                    quarantined.name.to_string_lossy()
                )
            })?;
            Ok(quarantined.size)
        }
    }

    fn clear_recursive_with_hook(
        directory: &std::os::fd::OwnedFd,
        before_quarantine: &mut impl FnMut(&std::os::fd::OwnedFd, &SecuredDirectoryEntry) -> Result<()>,
    ) -> Result<()> {
        for entry in Self::entries_for(directory)? {
            Self::remove_entry_recursive_with_hook(directory, &entry, before_quarantine)?;
        }
        Ok(())
    }

    fn clear_recursive(directory: &std::os::fd::OwnedFd) -> Result<()> {
        Self::clear_recursive_with_hook(directory, &mut |_, _| Ok(()))
    }

    fn prune_incremental_recursive(directory: &std::os::fd::OwnedFd) -> Result<()> {
        use rustix::fs::FileType;

        for entry in Self::entries_for(directory)? {
            Self::revalidate_entry(directory, &entry)?;
            if !FileType::from_raw_mode(entry.mode).is_dir() {
                continue;
            }
            let child = Self::open_child_dir(directory, &entry)?;
            if entry.name == "incremental" {
                drop(child);
                Self::remove_entry_recursive_with_hook(directory, &entry, &mut |_, _| Ok(()))?;
            } else {
                Self::prune_incremental_recursive(&child)?;
                Self::revalidate_entry(directory, &entry)?;
            }
        }
        Ok(())
    }

    pub(crate) fn size_bytes(&self) -> Result<u64> {
        self.revalidate_path()?;
        let size = Self::size_recursive(&self.directory)?;
        self.revalidate_path()?;
        Ok(size)
    }

    pub(crate) fn prune_incremental_dirs(&self) -> Result<()> {
        self.revalidate_path()?;
        Self::prune_incremental_recursive(&self.directory)?;
        self.revalidate_path()
    }

    pub(crate) fn clear_contents(&self) -> Result<()> {
        self.revalidate_path()?;
        Self::clear_recursive(&self.directory)?;
        self.revalidate_path()
    }

    #[cfg(test)]
    fn clear_contents_with_hook(
        &self,
        mut before_quarantine: impl FnMut(&std::os::fd::OwnedFd, &SecuredDirectoryEntry) -> Result<()>,
    ) -> Result<()> {
        self.revalidate_path()?;
        Self::clear_recursive_with_hook(&self.directory, &mut before_quarantine)?;
        self.revalidate_path()
    }

    pub(crate) fn clear_children_matching(
        &self,
        mut should_clear: impl FnMut(&std::ffi::OsStr) -> bool,
    ) -> Result<u64> {
        use rustix::fs::FileType;

        self.revalidate_path()?;
        let mut freed = 0u64;
        for entry in Self::entries_for(&self.directory)? {
            if !should_clear(&entry.name) {
                continue;
            }
            Self::revalidate_entry(&self.directory, &entry)?;
            if FileType::from_raw_mode(entry.mode).is_symlink() {
                // A numbered lane-cache symlink is not a cache we own. Leave
                // both the link and its referent untouched and fail closed.
                bail!(
                    "refusing to prune symlinked Cargo-cache entry {}",
                    entry.name.to_string_lossy()
                );
            } else {
                freed = freed.saturating_add(Self::remove_entry_recursive_with_hook(
                    &self.directory,
                    &entry,
                    &mut |_, _| Ok(()),
                )?);
            }
        }
        self.revalidate_path()?;
        Ok(freed)
    }

    pub(crate) fn validate_child_directories_matching(
        &self,
        mut should_validate: impl FnMut(&std::ffi::OsStr) -> bool,
        optional_nested_dir: Option<&str>,
    ) -> Result<()> {
        use rustix::fs::FileType;

        self.revalidate_path()?;
        for entry in Self::entries_for(&self.directory)? {
            if !should_validate(&entry.name) {
                continue;
            }
            Self::revalidate_entry(&self.directory, &entry)?;
            if !FileType::from_raw_mode(entry.mode).is_dir() {
                bail!(
                    "managed Cargo-cache path {} must be a real no-follow directory",
                    entry.name.to_string_lossy()
                );
            }
            let child = Self::open_child_dir(&self.directory, &entry)?;
            if let Some(nested_name) = optional_nested_dir {
                for nested in Self::entries_for(&child)? {
                    if nested.name != nested_name {
                        continue;
                    }
                    Self::revalidate_entry(&child, &nested)?;
                    if !FileType::from_raw_mode(nested.mode).is_dir() {
                        bail!(
                            "managed Cargo-cache path {}/{} must be a real no-follow directory",
                            entry.name.to_string_lossy(),
                            nested.name.to_string_lossy()
                        );
                    }
                    let nested_child = Self::open_child_dir(&child, &nested)?;
                    let nested_stat = rustix::fs::fstat(&nested_child)
                        .context("failed to validate nested Cargo-cache target")?;
                    if !Self::same_identity(&nested_stat, nested.dev, nested.ino) {
                        bail!(
                            "managed Cargo-cache nested target identity changed: {}/{}",
                            entry.name.to_string_lossy(),
                            nested.name.to_string_lossy()
                        );
                    }
                }
                Self::revalidate_entry(&self.directory, &entry)?;
            }
        }
        self.revalidate_path()
    }
}

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct SecuredRunRoot {
    path: PathBuf,
    directory: std::os::fd::OwnedFd,
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
fn secure_run_root(path: &Path, create_missing: bool) -> Result<Option<SecuredRunRoot>> {
    use rustix::fs::{fstat, mkdirat, open, openat, FileType, Mode, OFlags};
    use rustix::io::Errno;
    use rustix::process::geteuid;
    use std::path::Component;

    const GROUP_OR_OTHER_WRITE: u32 = 0o022;
    const PRIVATE_DIR_MODE: u32 = 0o700;
    const STICKY: u32 = 0o1000;

    let mut components = path.components();
    if !matches!(components.next(), Some(Component::RootDir)) {
        bail!("parallel run root must be absolute: {}", path.display());
    }
    let names = components
        .map(|component| match component {
            Component::Normal(name) => Ok(name.to_os_string()),
            _ => bail!(
                "parallel run root contains an unsafe component: {}",
                path.display()
            ),
        })
        .collect::<Result<Vec<_>>>()?;
    if names.is_empty() {
        bail!("refusing to treat the filesystem root as a parallel run root");
    }

    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let euid = geteuid().as_raw();
    let mut current = open("/", flags, Mode::empty())
        .context("failed to open filesystem root while securing parallel run root")?;
    let mut traversed = PathBuf::from("/");
    for (index, name) in names.iter().enumerate() {
        let current_stat = fstat(&current).with_context(|| {
            format!(
                "failed to inspect parallel-run ancestor {}",
                traversed.display()
            )
        })?;
        let current_mode = current_stat.st_mode & 0o7777;
        let root_owned_sticky = current_stat.st_uid == 0 && current_mode & STICKY != 0;
        if current_stat.st_uid != 0 && current_stat.st_uid != euid {
            bail!(
                "parallel-run ancestor {} is owned by untrusted uid {}",
                traversed.display(),
                current_stat.st_uid
            );
        }
        if current_mode & GROUP_OR_OTHER_WRITE != 0 && !root_owned_sticky {
            bail!(
                "parallel-run ancestor {} is group/other-writable (mode {current_mode:04o})",
                traversed.display()
            );
        }

        let child = match openat(&current, name, flags, Mode::empty()) {
            Ok(child) => child,
            Err(Errno::NOENT) if !create_missing => return Ok(None),
            Err(Errno::NOENT) => {
                match mkdirat(&current, name, Mode::from_raw_mode(PRIVATE_DIR_MODE)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(err) => {
                        return Err(err).with_context(|| {
                            format!(
                                "failed to create private parallel-run component {}",
                                traversed.join(name).display()
                            )
                        })
                    }
                }
                openat(&current, name, flags, Mode::empty()).with_context(|| {
                    format!(
                        "failed to open newly created parallel-run component {} without following symlinks",
                        traversed.join(name).display()
                    )
                })?
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "refusing parallel-run component {} that is not a no-follow directory (possible symlink)",
                        traversed.join(name).display()
                    )
                })
            }
        };
        traversed.push(name);
        if index + 1 == names.len() {
            let stat = fstat(&child).with_context(|| {
                format!("failed to inspect parallel run root {}", path.display())
            })?;
            let mode = stat.st_mode & 0o7777;
            if !FileType::from_raw_mode(stat.st_mode).is_dir()
                || stat.st_uid != euid
                || mode & GROUP_OR_OTHER_WRITE != 0
            {
                bail!(
                    "parallel run root {} must be an owned no-follow directory without group/other write permission (found uid {}, mode {mode:04o})",
                    path.display(),
                    stat.st_uid
                );
            }
            let secured = SecuredRunRoot {
                path: path.to_path_buf(),
                directory: child,
                dev: stat.st_dev,
                ino: stat.st_ino,
            };
            secured.revalidate_path()?;
            return Ok(Some(secured));
        }
        current = child;
    }
    unreachable!("an absolute non-root path has at least one component")
}

#[cfg(unix)]
pub(crate) fn prepare_parallel_run_root(path: &Path) -> Result<()> {
    use rustix::fs::{openat, unlinkat, AtFlags, Mode, OFlags};
    use std::io::Write as _;

    let secured = secure_run_root(path, true)?
        .with_context(|| format!("failed to create parallel run root {}", path.display()))?;
    secured.revalidate_path()?;
    let probe_name = format!(
        ".auto-run-root-write-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .context("system clock is before Unix epoch")?
            .as_nanos()
    );
    let probe_fd = openat(
        &secured.directory,
        probe_name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .with_context(|| {
        format!(
            "failed to create descriptor-relative write probe under {}",
            path.display()
        )
    })?;
    let mut probe = std::fs::File::from(probe_fd);
    if let Err(err) = probe.write_all(b"ok") {
        let _ = unlinkat(&secured.directory, probe_name.as_str(), AtFlags::empty());
        return Err(err).with_context(|| {
            format!(
                "failed to write descriptor-relative probe under {}",
                path.display()
            )
        });
    }
    drop(probe);
    unlinkat(&secured.directory, probe_name.as_str(), AtFlags::empty()).with_context(|| {
        format!(
            "failed to remove descriptor-relative write probe under {}",
            path.display()
        )
    })?;
    secured.revalidate_path()
}

#[cfg(not(unix))]
pub(crate) fn prepare_parallel_run_root(path: &Path) -> Result<()> {
    bail!(
        "parallel run root {} requires Unix descriptor-relative no-follow support; use a supported Unix host",
        path.display()
    )
}

#[cfg(unix)]
impl SecuredRunRoot {
    fn revalidate_path(&self) -> Result<()> {
        use rustix::fs::fstat;

        let held = fstat(&self.directory).with_context(|| {
            format!(
                "failed to re-inspect held parallel run root {}",
                self.path.display()
            )
        })?;
        if !SecuredCargoTargetDir::same_identity(&held, self.dev, self.ino) {
            bail!(
                "held parallel run-root identity changed for {}",
                self.path.display()
            );
        }
        let (_, reopened, _) = open_absolute_dir_no_follow(&self.path)?;
        let reopened_stat = fstat(&reopened).with_context(|| {
            format!(
                "failed to inspect reopened parallel run root {}",
                self.path.display()
            )
        })?;
        if !SecuredCargoTargetDir::same_identity(&reopened_stat, self.dev, self.ino) {
            bail!(
                "parallel run-root path was replaced after secure open: {}",
                self.path.display()
            );
        }
        Ok(())
    }
}

#[cfg(not(unix))]
pub(crate) fn ensure_safe_private_cargo_target_dir(path: &Path, owned_root: &Path) -> Result<()> {
    bail!(
        "run-owned Cargo target {} under {} requires Unix descriptor-relative no-follow support; use --cargo-target none or an inherited external CARGO_TARGET_DIR on this platform",
        path.display(),
        owned_root.display()
    )
}

#[cfg(unix)]
pub(crate) fn reset_managed_lane_root(lane_root: &Path, run_root: &Path) -> Result<()> {
    let lanes = run_root.join("lanes");
    let expected_parent = lane_root.parent().with_context(|| {
        format!(
            "lane root {} is not under a managed lanes directory",
            lane_root.display()
        )
    })?;
    if expected_parent != lanes {
        bail!(
            "lane root {} is not a direct child of {}",
            lane_root.display(),
            lanes.display()
        );
    }
    let lane_name = lane_root
        .file_name()
        .context("managed lane root has no file name")?;
    let secured_lanes = secure_private_cargo_target_dir(&lanes, run_root)?;
    secured_lanes.reset_managed_child_directory(lane_name)
}

#[cfg(not(unix))]
pub(crate) fn reset_managed_lane_root(lane_root: &Path, _run_root: &Path) -> Result<()> {
    bail!(
        "managed lane reset for {} requires Unix descriptor-relative no-follow support",
        lane_root.display()
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ParallelCargoTargetLayout {
    None,
    Fixed(String),
    LaneLocal,
}

pub(crate) fn resolve_parallel_cargo_target_layout(
    cargo_target: ParallelCargoTarget,
    inherited_cargo_target_dir: Option<&str>,
    max_concurrent_workers: usize,
    repo_uses_cargo: bool,
    run_root: &Path,
) -> ParallelCargoTargetLayout {
    let inherited = inherited_cargo_target_dir
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    match cargo_target {
        ParallelCargoTarget::None => ParallelCargoTargetLayout::None,
        ParallelCargoTarget::Shared => ParallelCargoTargetLayout::Fixed(
            run_root
                .join("shared-cargo-target")
                .to_string_lossy()
                .into_owned(),
        ),
        ParallelCargoTarget::Lane => {
            if max_concurrent_workers > 1 {
                ParallelCargoTargetLayout::LaneLocal
            } else {
                ParallelCargoTargetLayout::Fixed(
                    run_root.join("cargo-target").to_string_lossy().into_owned(),
                )
            }
        }
        ParallelCargoTarget::Auto => {
            if max_concurrent_workers > 1 && repo_uses_cargo {
                ParallelCargoTargetLayout::LaneLocal
            } else if let Some(target_dir) = inherited {
                ParallelCargoTargetLayout::Fixed(target_dir)
            } else {
                ParallelCargoTargetLayout::None
            }
        }
    }
}

pub(crate) fn cargo_target_prompt_clause(lane_local: bool, summary: Option<&str>) -> String {
    if lane_local {
        return "Use the host-provided `CARGO_TARGET_DIR`; this run gives each lane its own target directory, so final proofs should go through `cargo test` or the repo's verification wrapper rather than direct binaries from another lane. Do not override it.".to_string();
    }
    if summary.is_some() {
        return "Use the host-provided `CARGO_TARGET_DIR`. If Cargo is busy, wait or narrow the proof instead of switching target directories. Do not use direct target-dir test binaries as proof unless you just built that exact artifact from this lane's source tree.".to_string();
    }
    "Use the repo's normal Cargo target behavior. Do not create ad hoc target directories unless the task explicitly requires isolation, and prefer `cargo test` or the repo's verification wrapper for final proof.".to_string()
}

pub(crate) fn repo_uses_cargo(repo_root: &Path) -> bool {
    repo_root.join("Cargo.toml").exists()
}

pub(crate) fn install_parallel_worker_git_guard(
    extra_env: &mut Vec<(String, String)>,
    run_root: &Path,
) -> Result<()> {
    let guard_dir = run_root.join(WORKER_GIT_GUARD_DIR);
    fs::create_dir_all(&guard_dir)
        .with_context(|| format!("failed to create {}", guard_dir.display()))?;
    let guard_path = guard_dir.join("git");
    atomic_write(&guard_path, worker_git_guard_script().as_bytes())
        .with_context(|| format!("failed to write {}", guard_path.display()))?;
    make_executable(&guard_path)?;

    let real_git = resolve_real_git_for_worker_guard(run_root)
        .unwrap_or_else(|| PathBuf::from("/usr/bin/git"));
    upsert_env(extra_env, "AUTO_PARALLEL_GIT_GUARD", "remote-git-disabled");
    upsert_env(extra_env, "AUTO_REAL_GIT", &real_git.to_string_lossy());
    upsert_env(extra_env, "GIT_TERMINAL_PROMPT", "0");
    upsert_env(extra_env, "GIT_ASKPASS", "/bin/false");
    upsert_env(extra_env, "SSH_ASKPASS", "/bin/false");
    install_git_protocol_block_config(extra_env);

    let current_path = extra_env
        .iter()
        .rev()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| value.clone())
        .or_else(|| env::var("PATH").ok())
        .unwrap_or_default();
    let guarded_path = if current_path.trim().is_empty() {
        guard_dir.to_string_lossy().into_owned()
    } else {
        format!("{}:{current_path}", guard_dir.display())
    };
    upsert_env(extra_env, "PATH", &guarded_path);
    Ok(())
}

fn install_git_protocol_block_config(extra_env: &mut Vec<(String, String)>) {
    upsert_env(
        extra_env,
        "GIT_CONFIG_COUNT",
        &WORKER_GIT_GUARD_PROTOCOLS.len().to_string(),
    );
    for (index, protocol) in WORKER_GIT_GUARD_PROTOCOLS.iter().enumerate() {
        upsert_env(
            extra_env,
            &format!("GIT_CONFIG_KEY_{index}"),
            &format!("protocol.{protocol}.allow"),
        );
        upsert_env(extra_env, &format!("GIT_CONFIG_VALUE_{index}"), "never");
    }
}

fn worker_git_guard_script() -> String {
    let blocked_pattern = WORKER_GIT_GUARD_BLOCKED_VERBS.join("|");
    format!(
        r#"#!/bin/sh
verb=""
expect_value=0
for arg in "$@"; do
  if [ "$expect_value" = "1" ]; then
    expect_value=0
    continue
  fi
  case "$arg" in
    -C|-c|--git-dir|--work-tree|--namespace)
      expect_value=1
      continue
      ;;
    --git-dir=*|--work-tree=*|--namespace=*)
      continue
      ;;
    -*)
      continue
      ;;
    *)
      verb="$arg"
      break
      ;;
  esac
done

case "$verb" in
  {blocked_pattern})
    echo "AUTO_ENV_BLOCKER: auto parallel worker git guard blocked 'git $verb'; host owns remote sync and branch reconciliation" >&2
    exit 126
    ;;
esac

if [ -n "${{AUTO_REAL_GIT:-}}" ]; then
  exec "$AUTO_REAL_GIT" "$@"
fi
exec git "$@"
"#
    )
}

fn upsert_env(extra_env: &mut Vec<(String, String)>, key: &str, value: &str) {
    if let Some((_, existing)) = extra_env
        .iter_mut()
        .rev()
        .find(|(existing, _)| existing == key)
    {
        *existing = value.to_string();
    } else {
        extra_env.push((key.to_string(), value.to_string()));
    }
}

fn resolve_real_git_for_worker_guard(run_root: &Path) -> Option<PathBuf> {
    if let Some(path) = env::var_os("AUTO_REAL_GIT")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .filter(|path| path.exists())
    {
        return Some(path);
    }

    let path_env = env::var_os("PATH")?;
    for path_dir in env::split_paths(&path_env) {
        let candidate = path_dir.join("git");
        if candidate.starts_with(run_root) {
            continue;
        }
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .with_context(|| format!("failed to stat {}", path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .with_context(|| format!("failed to chmod {}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn effective_parallel_claude_max_turns(args: &ParallelArgs) -> Option<usize> {
    args.max_turns
}

pub(crate) fn default_cargo_build_jobs_for(
    available_parallelism: usize,
    max_concurrent_workers: usize,
) -> usize {
    let available_parallelism = available_parallelism.max(1);
    let workers = max_concurrent_workers.max(1);
    (available_parallelism / (workers + 1)).clamp(1, 4)
}

#[cfg(test)]
mod tests {
    use super::{make_executable, worker_git_guard_script, WORKER_GIT_GUARD_DIR};
    use crate::parallel_command::*;
    use crate::util::output_retrying_etxtbsy;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::time::UNIX_EPOCH;

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
    }

    fn test_parallel_args(
        max_concurrent_workers: usize,
        cargo_build_jobs: Option<usize>,
        cargo_target: ParallelCargoTarget,
    ) -> ParallelArgs {
        ParallelArgs {
            action: None,
            apply_receipt_backfill_handoffs: false,
            json: false,
            max_iterations: None,
            max_concurrent_workers,
            cargo_build_jobs,
            cargo_target,
            prompt_file: None,
            model: "gpt-5.6-sol".to_string(),
            reasoning_effort: "high".to_string(),
            branch: None,
            reference_repos: Vec::new(),
            include_siblings: false,
            run_root: None,
            codex_bin: PathBuf::from("codex"),
            claude: false,
            max_turns: None,
            max_retries: 2,
        }
    }

    #[test]
    fn parallel_run_root_resolves_relative_override_under_repo_root() {
        let args = ParallelArgs {
            action: None,
            apply_receipt_backfill_handoffs: false,
            json: false,
            max_iterations: None,
            max_concurrent_workers: 5,
            cargo_build_jobs: None,
            cargo_target: ParallelCargoTarget::Auto,
            prompt_file: None,
            model: "gpt-5.6-sol".to_string(),
            reasoning_effort: "high".to_string(),
            branch: None,
            reference_repos: Vec::new(),
            include_siblings: false,
            run_root: Some(PathBuf::from(".auto/super/run-1")),
            codex_bin: PathBuf::from("codex"),
            claude: false,
            max_turns: None,
            max_retries: 2,
        };

        assert_eq!(
            parallel_run_root(&PathBuf::from("/repo"), &args),
            PathBuf::from("/repo/.auto/super/run-1")
        );
    }

    #[test]
    fn default_cargo_build_jobs_caps_nested_parallelism() {
        assert_eq!(default_cargo_build_jobs_for(22, 1), 4);
        assert_eq!(default_cargo_build_jobs_for(22, 5), 3);
        assert_eq!(default_cargo_build_jobs_for(12, 4), 2);
        assert_eq!(default_cargo_build_jobs_for(3, 2), 1);
        assert_eq!(default_cargo_build_jobs_for(1, 1), 1);
    }

    #[test]
    fn loop_worker_env_respects_override_and_inherited_cargo_jobs() {
        let run_root = unique_temp_dir("loop-worker-env");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let shared_target = run_root
            .join("shared-cargo-target")
            .to_string_lossy()
            .into_owned();

        let inherited = resolve_loop_worker_env(
            None,
            ParallelCargoTarget::Auto,
            Some("8"),
            None,
            22,
            5,
            true,
            &run_root,
        )
        .unwrap();
        assert_eq!(
            inherited.extra_env,
            vec![("CARGO_BUILD_JOBS".to_string(), "8".to_string())]
        );
        assert_eq!(inherited.cargo_jobs_summary, "inherited CARGO_BUILD_JOBS=8");
        assert!(inherited.lane_local_cargo_target);
        assert!(inherited
            .cargo_target_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("lane-local")));
        assert_eq!(
            inherited.host_cargo_env.value("CARGO_TARGET_DIR"),
            Some(
                run_root
                    .join("host-cargo-target")
                    .to_string_lossy()
                    .as_ref()
            )
        );
        assert_eq!(
            inherited.host_cargo_env.value("CARGO_BUILD_JOBS"),
            Some("8")
        );

        let overridden = resolve_loop_worker_env(
            Some(3),
            ParallelCargoTarget::Auto,
            Some("8"),
            None,
            22,
            5,
            true,
            &run_root,
        )
        .unwrap();
        assert_eq!(
            overridden.extra_env,
            vec![("CARGO_BUILD_JOBS".to_string(), "3".to_string())]
        );
        assert_eq!(overridden.cargo_jobs_summary, "override CARGO_BUILD_JOBS=3");
        assert!(overridden.lane_local_cargo_target);
        assert_eq!(
            overridden.host_cargo_env.value("CARGO_BUILD_JOBS"),
            Some("3")
        );

        let automatic = resolve_loop_worker_env(
            None,
            ParallelCargoTarget::Auto,
            None,
            None,
            22,
            5,
            true,
            &run_root,
        )
        .unwrap();
        assert_eq!(
            automatic.extra_env,
            vec![("CARGO_BUILD_JOBS".to_string(), "3".to_string())]
        );
        assert_eq!(automatic.cargo_jobs_summary, "auto CARGO_BUILD_JOBS=3");
        assert!(automatic.lane_local_cargo_target);
        assert_eq!(
            automatic.host_cargo_env.value("CARGO_BUILD_JOBS"),
            Some("3")
        );

        let shared = resolve_loop_worker_env(
            None,
            ParallelCargoTarget::Shared,
            None,
            None,
            22,
            5,
            true,
            &run_root,
        )
        .unwrap();
        assert_eq!(
            shared.extra_env,
            vec![
                ("CARGO_TARGET_DIR".to_string(), shared_target),
                ("CARGO_BUILD_JOBS".to_string(), "3".to_string())
            ]
        );
        assert!(!shared.lane_local_cargo_target);
        assert_eq!(
            shared.host_cargo_env.value("CARGO_TARGET_DIR"),
            shared.cargo_target_summary.as_deref()
        );
        assert_eq!(shared.host_cargo_env.value("CARGO_BUILD_JOBS"), Some("3"));
        assert!(
            shared
                .host_cargo_env
                .values()
                .all(|(key, _)| key.starts_with("CARGO_")),
            "host commands must never inherit worker git-guard variables"
        );

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[cfg(unix)]
    #[test]
    fn build_loop_worker_env_creates_and_repairs_run_owned_targets_as_private() {
        let repo_root = unique_temp_dir("private-cargo-repo");
        let run_root = unique_temp_dir("private-cargo-run");
        fs::create_dir_all(&repo_root).expect("create repo root");
        fs::write(repo_root.join("Cargo.toml"), "[workspace]\nmembers = []\n")
            .expect("write manifest");
        fs::create_dir_all(&run_root).expect("create run root");
        let mut run_permissions = fs::metadata(&run_root)
            .expect("stat run root")
            .permissions();
        run_permissions.set_mode(0o755);
        fs::set_permissions(&run_root, run_permissions).expect("make run root safely traversable");

        let shared_target = run_root.join("shared-cargo-target");
        fs::create_dir_all(&shared_target).expect("precreate shared target");
        let mut permissions = fs::metadata(&shared_target)
            .expect("stat shared target")
            .permissions();
        permissions.set_mode(0o775);
        fs::set_permissions(&shared_target, permissions).expect("make target non-private");

        let shared = build_loop_worker_env(
            &test_parallel_args(3, Some(2), ParallelCargoTarget::Shared),
            &repo_root,
            &run_root,
        )
        .expect("shared worker env");
        assert_eq!(
            fs::metadata(&run_root)
                .expect("stat repaired run root")
                .permissions()
                .mode()
                & 0o777,
            0o755,
            "the operator-selected run root must not be chmodded"
        );
        assert_eq!(
            fs::metadata(&shared_target)
                .expect("stat repaired shared target")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            shared.host_cargo_env.value("CARGO_TARGET_DIR"),
            Some(shared_target.to_string_lossy().as_ref())
        );

        let single_lane_target = run_root.join("cargo-target");
        let lane = build_loop_worker_env(
            &test_parallel_args(1, Some(4), ParallelCargoTarget::Lane),
            &repo_root,
            &run_root,
        )
        .expect("single-lane worker env");
        assert_eq!(
            fs::metadata(&single_lane_target)
                .expect("stat single-lane target")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            lane.host_cargo_env.value("CARGO_TARGET_DIR"),
            Some(single_lane_target.to_string_lossy().as_ref())
        );

        let host_target = run_root.join("host-cargo-target");
        let lane_local = build_loop_worker_env(
            &test_parallel_args(3, Some(2), ParallelCargoTarget::Lane),
            &repo_root,
            &run_root,
        )
        .expect("multi-lane worker env");
        assert_eq!(
            fs::metadata(&host_target)
                .expect("stat host target")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            lane_local.host_cargo_env.value("CARGO_TARGET_DIR"),
            Some(host_target.to_string_lossy().as_ref())
        );

        fs::remove_dir_all(&repo_root).expect("remove repo root");
        fs::remove_dir_all(&run_root).expect("remove run root");
    }

    #[cfg(unix)]
    #[test]
    fn run_owned_target_symlink_fails_closed_and_inherited_target_is_not_chmodded() {
        let root = unique_temp_dir("private-cargo-boundary");
        let run_root = root.join("run");
        let outside = root.join("outside");
        fs::create_dir_all(&run_root).expect("create run root");
        fs::create_dir_all(&outside).expect("create outside target");
        let mut root_permissions = fs::metadata(&root).expect("stat test root").permissions();
        root_permissions.set_mode(0o755);
        fs::set_permissions(&root, root_permissions).expect("secure test ancestor");
        let mut run_permissions = fs::metadata(&run_root)
            .expect("stat run root")
            .permissions();
        run_permissions.set_mode(0o755);
        fs::set_permissions(&run_root, run_permissions).expect("secure owned root");
        let mut outside_permissions = fs::metadata(&outside).expect("stat outside").permissions();
        outside_permissions.set_mode(0o775);
        fs::set_permissions(&outside, outside_permissions).expect("chmod outside");
        std::os::unix::fs::symlink(&outside, run_root.join("shared-cargo-target"))
            .expect("create target symlink");

        let shared = resolve_loop_worker_env(
            Some(2),
            ParallelCargoTarget::Shared,
            None,
            None,
            8,
            2,
            true,
            &run_root,
        )
        .expect("resolve shared target");
        let error = prepare_run_owned_cargo_targets(&shared, &run_root)
            .expect_err("run-owned target symlink must fail closed");
        assert!(format!("{error:#}").contains("symlink"), "{error:#}");

        let inherited = resolve_loop_worker_env(
            Some(2),
            ParallelCargoTarget::Auto,
            None,
            Some(outside.to_string_lossy().as_ref()),
            8,
            1,
            true,
            &run_root,
        )
        .expect("resolve inherited target");
        prepare_run_owned_cargo_targets(&inherited, &run_root).expect("no inherited chmod");
        assert_eq!(
            fs::metadata(&outside)
                .expect("stat outside after preparation")
                .permissions()
                .mode()
                & 0o777,
            0o775,
            "Autodev must never chmod an inherited external target"
        );

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn run_owned_target_rejects_intermediate_symlink_without_touching_external_directory() {
        let root = unique_temp_dir("private-cargo-intermediate-symlink");
        let safe = root.join("safe");
        let outside = root.join("outside");
        fs::create_dir_all(&safe).expect("create safe parent");
        fs::create_dir_all(&outside).expect("create outside");
        let mut root_permissions = fs::metadata(&root).expect("stat test root").permissions();
        root_permissions.set_mode(0o755);
        fs::set_permissions(&root, root_permissions).expect("secure test ancestor");
        let mut outside_permissions = fs::metadata(&outside).expect("stat outside").permissions();
        outside_permissions.set_mode(0o775);
        fs::set_permissions(&outside, outside_permissions).expect("chmod outside");
        std::os::unix::fs::symlink(&outside, safe.join("pivot"))
            .expect("create intermediate symlink");

        let error =
            ensure_private_cargo_target_dir(&safe.join("pivot/cargo-target"), &safe.join("pivot"))
                .expect_err("intermediate symlink must fail closed");
        assert!(format!("{error:#}").contains("symlink"), "{error:#}");
        assert!(
            !outside.join("cargo-target").exists(),
            "descriptor-relative traversal must not create through the symlink"
        );
        assert_eq!(
            fs::metadata(&outside)
                .expect("stat untouched outside")
                .permissions()
                .mode()
                & 0o777,
            0o775,
            "the external directory must never be chmodded"
        );

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn secured_target_revalidation_rejects_path_swap_before_recursive_clear() {
        let root = unique_temp_dir("private-cargo-target-swap");
        let run_root = root.join("run");
        let target = run_root.join("lane-caches/lane-1/cargo-target");
        let moved = run_root.join("lane-caches/lane-1/moved-target");
        let outside = root.join("outside");
        fs::create_dir_all(target.join("debug")).expect("create target");
        fs::create_dir_all(&outside).expect("create outside");
        let mut root_permissions = fs::metadata(&root).expect("stat root").permissions();
        root_permissions.set_mode(0o755);
        fs::set_permissions(&root, root_permissions).expect("secure root");
        let mut run_permissions = fs::metadata(&run_root)
            .expect("stat run root")
            .permissions();
        run_permissions.set_mode(0o755);
        fs::set_permissions(&run_root, run_permissions).expect("secure run root");
        fs::write(target.join("debug/local.bin"), b"local").expect("write local");
        fs::write(outside.join("sentinel.bin"), b"external").expect("write outside");

        let secured =
            secure_private_cargo_target_dir(&target, &run_root).expect("secure target descriptor");
        fs::rename(&target, &moved).expect("move secured target");
        std::os::unix::fs::symlink(&outside, &target).expect("replace target with symlink");

        let error = secured
            .clear_contents()
            .expect_err("replaced target path must fail closed");
        assert!(
            format!("{error:#}").contains("identity") || format!("{error:#}").contains("no-follow"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(moved.join("debug/local.bin")).expect("moved target survives"),
            b"local"
        );
        assert_eq!(
            fs::read(outside.join("sentinel.bin")).expect("outside survives"),
            b"external"
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn secured_target_retains_validated_descriptors_across_former_reopen_swap_seam() {
        let root = unique_temp_dir("private-cargo-validated-open-swap");
        let run_root = root.join("run");
        let target = run_root.join("lane-caches/lane-1/cargo-target");
        let moved = run_root.join("lane-caches/lane-1/moved-target");
        let outside = root.join("outside");
        fs::create_dir_all(target.join("debug")).expect("create target");
        fs::create_dir_all(&outside).expect("create outside");
        let mut root_permissions = fs::metadata(&root).expect("stat root").permissions();
        root_permissions.set_mode(0o755);
        fs::set_permissions(&root, root_permissions).expect("secure root");
        let mut run_permissions = fs::metadata(&run_root)
            .expect("stat run root")
            .permissions();
        run_permissions.set_mode(0o755);
        fs::set_permissions(&run_root, run_permissions).expect("secure run root");
        fs::write(target.join("debug/local.bin"), b"local").expect("write local");
        fs::write(outside.join("sentinel.bin"), b"external").expect("write outside");

        let error = secure_private_cargo_target_dir_with_hook(&target, &run_root, || {
            fs::rename(&target, &moved).expect("move validated target");
            std::os::unix::fs::symlink(&outside, &target).expect("replace target with symlink");
            Ok(())
        })
        .expect_err("swap after validated open must fail closed");

        assert!(
            format!("{error:#}").contains("identity") || format!("{error:#}").contains("no-follow"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(moved.join("debug/local.bin")).expect("validated target survives"),
            b"local"
        );
        assert_eq!(
            fs::read(outside.join("sentinel.bin")).expect("outside survives"),
            b"external"
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn secured_clear_quarantines_and_rechecks_entry_after_deterministic_swap() {
        let root = unique_temp_dir("private-cargo-entry-delete-swap");
        let run_root = root.join("run");
        let target = run_root.join("lane-caches/lane-1/cargo-target");
        let outside = root.join("outside");
        fs::create_dir_all(&target).expect("create target");
        fs::create_dir_all(&outside).expect("create outside");
        let mut root_permissions = fs::metadata(&root).expect("stat root").permissions();
        root_permissions.set_mode(0o755);
        fs::set_permissions(&root, root_permissions).expect("secure root");
        let mut run_permissions = fs::metadata(&run_root)
            .expect("stat run root")
            .permissions();
        run_permissions.set_mode(0o755);
        fs::set_permissions(&run_root, run_permissions).expect("secure run root");
        fs::write(target.join("victim"), b"managed").expect("write managed victim");
        fs::write(outside.join("sentinel"), b"outside").expect("write outside sentinel");

        let secured =
            secure_private_cargo_target_dir(&target, &run_root).expect("secure target descriptor");
        let mut swapped = false;
        let error = secured
            .clear_contents_with_hook(|directory, entry| {
                if !swapped && entry.name == "victim" {
                    use rustix::fs::{renameat_with, RenameFlags};
                    std::os::unix::fs::symlink(&outside, target.join("replacement"))
                        .expect("create replacement symlink");
                    renameat_with(
                        directory,
                        "victim",
                        directory,
                        "replacement",
                        RenameFlags::EXCHANGE,
                    )
                    .expect("exchange entry after validation");
                    swapped = true;
                }
                Ok(())
            })
            .expect_err("identity-changing exchange must fail before unlink");

        assert!(format!("{error:#}").contains("identity"), "{error:#}");
        assert_eq!(
            fs::read(outside.join("sentinel")).expect("outside sentinel survives"),
            b"outside"
        );
        assert!(target.join("victim").is_symlink());
        assert_eq!(
            fs::read(target.join("replacement")).expect("managed inode survives under replacement"),
            b"managed"
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn secured_clear_rechecks_quarantined_inode_after_pre_unlink_exchange() {
        let root = unique_temp_dir("private-cargo-entry-pre-unlink-swap");
        let run_root = root.join("run");
        let target = run_root.join("lane-caches/lane-1/cargo-target");
        let outside = root.join("outside");
        fs::create_dir_all(&target).expect("create target");
        fs::create_dir_all(&outside).expect("create outside");
        let mut root_permissions = fs::metadata(&root).expect("stat root").permissions();
        root_permissions.set_mode(0o755);
        fs::set_permissions(&root, root_permissions).expect("secure root");
        let mut run_permissions = fs::metadata(&run_root)
            .expect("stat run root")
            .permissions();
        run_permissions.set_mode(0o755);
        fs::set_permissions(&run_root, run_permissions).expect("secure run root");
        fs::write(target.join("victim"), b"managed").expect("write managed victim");
        fs::write(outside.join("sentinel"), b"outside").expect("write outside sentinel");

        let secured =
            secure_private_cargo_target_dir(&target, &run_root).expect("secure target descriptor");
        let mut swapped = false;
        let error = secured
            .clear_contents_with_hook(|directory, entry| {
                if !swapped
                    && entry
                        .name
                        .to_string_lossy()
                        .starts_with(".auto-secured-delete-")
                {
                    use rustix::fs::{renameat_with, RenameFlags};
                    std::os::unix::fs::symlink(&outside, target.join("replacement"))
                        .expect("create replacement symlink");
                    renameat_with(
                        directory,
                        &entry.name,
                        directory,
                        "replacement",
                        RenameFlags::EXCHANGE,
                    )
                    .expect("exchange quarantined inode before unlink");
                    swapped = true;
                }
                Ok(())
            })
            .expect_err("pre-unlink exchange must fail before unlink");

        assert!(format!("{error:#}").contains("identity"), "{error:#}");
        assert_eq!(
            fs::read(outside.join("sentinel")).expect("outside sentinel survives"),
            b"outside"
        );
        assert_eq!(
            fs::read(target.join("replacement")).expect("managed inode survives under replacement"),
            b"managed"
        );
        assert!(
            fs::read_dir(&target)
                .expect("read target")
                .any(|entry| entry
                    .expect("read entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".auto-secured-delete-")),
            "unexpected replacement must remain quarantined rather than unlinked"
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn run_owned_target_rejects_group_writable_ancestor_before_creation() {
        let root = unique_temp_dir("private-cargo-writable-ancestor");
        fs::create_dir_all(&root).expect("create unsafe ancestor");
        let mut permissions = fs::metadata(&root).expect("stat ancestor").permissions();
        permissions.set_mode(0o770);
        fs::set_permissions(&root, permissions).expect("make ancestor group writable");
        let owned_root = root.join("run");
        fs::create_dir_all(&owned_root).expect("create run-owned boundary");
        let target = owned_root.join("nested/cargo-target");

        let error = ensure_private_cargo_target_dir(&target, &owned_root)
            .expect_err("group-writable ancestor must fail closed");
        assert!(
            format!("{error:#}").contains("group/other-writable"),
            "{error:#}"
        );
        assert!(
            !owned_root.join("nested").exists(),
            "validation must happen before creating descendants"
        );
        assert_eq!(
            fs::metadata(&root)
                .expect("stat unchanged ancestor")
                .permissions()
                .mode()
                & 0o777,
            0o770,
            "Autodev must not silently chmod an ancestor it does not own as a run artifact"
        );

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn run_owned_target_rejects_group_writable_owned_root_without_chmodding_it() {
        let root = unique_temp_dir("private-cargo-writable-owned-root");
        let owned_root = root.join("run");
        fs::create_dir_all(&owned_root).expect("create owned root");
        let mut root_permissions = fs::metadata(&root).expect("stat test root").permissions();
        root_permissions.set_mode(0o755);
        fs::set_permissions(&root, root_permissions).expect("secure test ancestor");
        let mut owned_permissions = fs::metadata(&owned_root)
            .expect("stat owned root")
            .permissions();
        owned_permissions.set_mode(0o775);
        fs::set_permissions(&owned_root, owned_permissions).expect("make owned root unsafe");

        let target = owned_root.join("cargo-target");
        let error = ensure_private_cargo_target_dir(&target, &owned_root)
            .expect_err("an unsafe operator-selected root must fail closed");
        assert!(
            format!("{error:#}").contains("owned root")
                && format!("{error:#}").contains("group/other-writable"),
            "{error:#}"
        );
        assert!(!target.exists(), "target must not be created");
        assert_eq!(
            fs::metadata(&owned_root)
                .expect("stat unchanged owned root")
                .permissions()
                .mode()
                & 0o777,
            0o775,
            "Autodev must never chmod the operator-selected root"
        );

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn parallel_run_root_prepare_creates_missing_private_components_and_cleans_probe() {
        let root = unique_temp_dir("parallel-run-root-create");
        fs::create_dir_all(&root).expect("create root");
        let mut root_permissions = fs::metadata(&root).expect("stat root").permissions();
        root_permissions.set_mode(0o755);
        fs::set_permissions(&root, root_permissions).expect("secure root");
        let run_root = root.join("missing/nested/run");

        prepare_parallel_run_root(&run_root).expect("prepare missing run root");

        for path in [
            root.join("missing"),
            root.join("missing/nested"),
            run_root.clone(),
        ] {
            assert_eq!(
                fs::metadata(&path)
                    .unwrap_or_else(|_| panic!("stat {}", path.display()))
                    .permissions()
                    .mode()
                    & 0o777,
                0o700,
                "new run-root components must be private"
            );
        }
        assert!(
            fs::read_dir(&run_root)
                .expect("read prepared run root")
                .all(|entry| {
                    !entry
                        .expect("read entry")
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".auto-run-root-write-test-")
                }),
            "descriptor-relative write probe must be removed"
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn parallel_run_root_prepare_rejects_symlink_without_touching_external_directory() {
        let root = unique_temp_dir("parallel-run-root-symlink");
        let outside = root.join("outside");
        let run_root = root.join("run");
        fs::create_dir_all(&outside).expect("create outside");
        let mut root_permissions = fs::metadata(&root).expect("stat root").permissions();
        root_permissions.set_mode(0o755);
        fs::set_permissions(&root, root_permissions).expect("secure root");
        fs::write(outside.join("sentinel.bin"), b"external").expect("write sentinel");
        std::os::unix::fs::symlink(&outside, &run_root).expect("symlink run root");

        let error =
            prepare_parallel_run_root(&run_root).expect_err("symlinked run root must fail closed");
        assert!(format!("{error:#}").contains("symlink"), "{error:#}");
        assert_eq!(
            fs::read(outside.join("sentinel.bin")).expect("read external sentinel"),
            b"external"
        );
        assert!(
            fs::read_dir(&outside).expect("read outside").all(|entry| {
                !entry
                    .expect("read entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".auto-run-root-write-test-")
            }),
            "write probe must never be created through the symlink"
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn managed_lane_reset_rejects_post_open_swap_before_recursive_mutation() {
        let root = unique_temp_dir("managed-lane-post-open-swap");
        let run_root = root.join("run");
        let lanes = run_root.join("lanes");
        let lane = lanes.join("lane-1");
        let moved = lanes.join("lane-1-moved");
        let outside = root.join("outside");
        fs::create_dir_all(lane.join("repo")).expect("create lane");
        fs::create_dir_all(&outside).expect("create outside");
        let mut root_permissions = fs::metadata(&root).expect("stat root").permissions();
        root_permissions.set_mode(0o755);
        fs::set_permissions(&root, root_permissions).expect("secure root");
        let mut run_permissions = fs::metadata(&run_root)
            .expect("stat run root")
            .permissions();
        run_permissions.set_mode(0o755);
        fs::set_permissions(&run_root, run_permissions).expect("secure run root");
        fs::write(lane.join("repo/managed"), b"managed").expect("write lane sentinel");
        fs::write(outside.join("sentinel"), b"outside").expect("write outside sentinel");

        let secured_lanes =
            secure_private_cargo_target_dir(&lanes, &run_root).expect("secure lanes descriptor");
        let error = secured_lanes
            .reset_managed_child_directory_with_hook(std::ffi::OsStr::new("lane-1"), || {
                fs::rename(&lane, &moved).expect("move validated lane");
                std::os::unix::fs::symlink(&outside, &lane)
                    .expect("replace lane with external symlink");
                Ok(())
            })
            .expect_err("post-open lane swap must fail before clear");

        assert!(format!("{error:#}").contains("identity"), "{error:#}");
        assert_eq!(
            fs::read(moved.join("repo/managed")).expect("managed lane survives"),
            b"managed"
        );
        assert_eq!(
            fs::read(outside.join("sentinel")).expect("outside survives"),
            b"outside"
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn managed_lane_creation_rejects_post_open_swap_before_returning() {
        let root = unique_temp_dir("managed-new-lane-post-open-swap");
        let run_root = root.join("run");
        let lanes = run_root.join("lanes");
        let lane = lanes.join("lane-1");
        let moved = lanes.join("lane-1-moved");
        let outside = root.join("outside");
        fs::create_dir_all(&lanes).expect("create managed lanes root");
        fs::create_dir_all(&outside).expect("create outside");
        let mut root_permissions = fs::metadata(&root).expect("stat root").permissions();
        root_permissions.set_mode(0o755);
        fs::set_permissions(&root, root_permissions).expect("secure root");
        let mut run_permissions = fs::metadata(&run_root)
            .expect("stat run root")
            .permissions();
        run_permissions.set_mode(0o755);
        fs::set_permissions(&run_root, run_permissions).expect("secure run root");
        fs::write(outside.join("sentinel"), b"outside").expect("write outside sentinel");

        let secured_lanes =
            secure_private_cargo_target_dir(&lanes, &run_root).expect("secure lanes descriptor");
        let error = secured_lanes
            .reset_managed_child_directory_with_hook(std::ffi::OsStr::new("lane-1"), || {
                fs::rename(&lane, &moved).expect("move newly created lane");
                std::os::unix::fs::symlink(&outside, &lane)
                    .expect("replace new lane with external symlink");
                Ok(())
            })
            .expect_err("post-open new-lane swap must fail before returning");

        assert!(format!("{error:#}").contains("identity"), "{error:#}");
        assert!(
            moved.is_dir(),
            "the held newly created lane must survive under its moved name"
        );
        assert_eq!(
            fs::read(outside.join("sentinel")).expect("outside survives"),
            b"outside"
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn loop_worker_env_rejects_zero_cargo_jobs_override() {
        let run_root = unique_temp_dir("loop-worker-env-error");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let err = resolve_loop_worker_env(
            Some(0),
            ParallelCargoTarget::Auto,
            None,
            None,
            22,
            5,
            true,
            &run_root,
        )
        .unwrap_err();
        assert!(err.to_string().contains("--cargo-build-jobs"));
        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn build_loop_worker_env_installs_git_guard() {
        let repo_root = unique_temp_dir("loop-worker-env-git-guard-repo");
        let run_root = unique_temp_dir("loop-worker-env-git-guard-run");
        fs::create_dir_all(&repo_root).expect("failed to create repo root");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let args = ParallelArgs {
            action: None,
            apply_receipt_backfill_handoffs: false,
            json: false,
            max_iterations: None,
            max_concurrent_workers: 2,
            cargo_build_jobs: None,
            cargo_target: ParallelCargoTarget::None,
            prompt_file: None,
            model: "gpt-5.6-sol".to_string(),
            reasoning_effort: "high".to_string(),
            branch: None,
            reference_repos: Vec::new(),
            include_siblings: false,
            run_root: None,
            codex_bin: PathBuf::from("codex"),
            claude: false,
            max_turns: None,
            max_retries: 2,
        };

        let env = build_loop_worker_env(&args, &repo_root, &run_root)
            .expect("worker env should include git guard");
        let guard_dir = run_root.join(WORKER_GIT_GUARD_DIR);
        let guard_path = guard_dir.join("git");
        assert!(guard_path.exists(), "missing {}", guard_path.display());
        assert_eq!(
            env.extra_env
                .iter()
                .find(|(key, _)| key == "AUTO_PARALLEL_GIT_GUARD")
                .map(|(_, value)| value.as_str()),
            Some("remote-git-disabled")
        );
        assert!(env
            .extra_env
            .iter()
            .find(|(key, _)| key == "PATH")
            .is_some_and(|(_, value)| value.starts_with(&format!("{}:", guard_dir.display()))));
        assert_eq!(
            env.extra_env
                .iter()
                .find(|(key, _)| key == "GIT_CONFIG_COUNT")
                .map(|(_, value)| value.as_str()),
            Some("4")
        );
        assert!(env
            .extra_env
            .iter()
            .any(|(key, value)| { key == "GIT_CONFIG_KEY_0" && value == "protocol.ssh.allow" }));
        assert!(
            env.extra_env
                .iter()
                .all(|(key, _)| key != super::CARGO_TARGET_DIR_ENV),
            "--cargo-target none must not override the worker's ambient target"
        );
        assert_eq!(
            env.host_cargo_env.value(super::CARGO_TARGET_DIR_ENV),
            None,
            "--cargo-target none must not override the host's ambient target"
        );

        fs::remove_dir_all(&repo_root).expect("failed to remove repo root");
        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn git_guard_script_blocks_remote_sync_verbs_before_real_git() {
        let run_root = unique_temp_dir("loop-worker-env-git-guard-script");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let guard_path = run_root.join("git");
        atomic_write(&guard_path, worker_git_guard_script().as_bytes())
            .expect("failed to write guard");
        make_executable(&guard_path).expect("failed to chmod guard");

        let mut blocked_command = Command::new(&guard_path);
        blocked_command
            .arg("-C")
            .arg("/tmp/repo")
            .arg("push")
            .arg("origin")
            .arg("main")
            .env("AUTO_REAL_GIT", "/bin/echo");
        let blocked = output_retrying_etxtbsy(&mut blocked_command).expect("guard should run");
        assert_eq!(blocked.status.code(), Some(126));
        assert!(String::from_utf8_lossy(&blocked.stderr).contains("AUTO_ENV_BLOCKER"));

        let mut allowed_command = Command::new(&guard_path);
        allowed_command
            .arg("status")
            .env("AUTO_REAL_GIT", "/bin/echo");
        let allowed = output_retrying_etxtbsy(&mut allowed_command).expect("guard should delegate");
        assert!(allowed.status.success());
        assert_eq!(String::from_utf8_lossy(&allowed.stdout).trim(), "status");

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn git_guard_env_blocks_absolute_git_network_transport() {
        let run_root = unique_temp_dir("loop-worker-env-git-guard-protocol");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let mut extra_env = Vec::new();
        install_parallel_worker_git_guard(&mut extra_env, &run_root)
            .expect("git guard should install");
        let real_git = extra_env
            .iter()
            .find(|(key, _)| key == "AUTO_REAL_GIT")
            .map(|(_, value)| value.clone())
            .expect("guard should record real git");

        let blocked = Command::new(real_git)
            .arg("ls-remote")
            .arg("https://example.com/repo.git")
            .envs(extra_env.iter().map(|(key, value)| (key, value)))
            .output()
            .expect("absolute git should run");
        assert!(!blocked.status.success());
        let stderr = String::from_utf8_lossy(&blocked.stderr);
        assert!(
            stderr.contains("transport 'https' not allowed")
                || stderr.contains("transport 'https' is not allowed"),
            "{stderr}"
        );

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn loop_worker_env_uses_lane_local_target_for_multi_lane_rust_runs() {
        let run_root = unique_temp_dir("loop-worker-env-inherited-target");
        fs::create_dir_all(&run_root).expect("failed to create run root");

        let env = resolve_loop_worker_env(
            None,
            ParallelCargoTarget::Auto,
            None,
            Some("/tmp/shared-target"),
            22,
            5,
            true,
            &run_root,
        )
        .expect("worker env should resolve");
        assert_eq!(
            env.extra_env,
            vec![("CARGO_BUILD_JOBS".to_string(), "3".to_string())]
        );
        assert!(
            env.cargo_target_summary
                .as_deref()
                .is_some_and(|summary| summary.contains("lane-local")),
            "multi-lane Rust runs should not inherit shared CARGO_TARGET_DIR"
        );
        assert!(env.lane_local_cargo_target);

        let single_lane = resolve_loop_worker_env(
            None,
            ParallelCargoTarget::Auto,
            None,
            Some("/tmp/shared-target"),
            22,
            1,
            true,
            &run_root,
        )
        .expect("worker env should resolve");
        assert_eq!(
            single_lane.extra_env,
            vec![
                (
                    "CARGO_TARGET_DIR".to_string(),
                    "/tmp/shared-target".to_string()
                ),
                ("CARGO_BUILD_JOBS".to_string(), "4".to_string())
            ]
        );
        assert_eq!(
            single_lane.cargo_target_summary,
            Some("/tmp/shared-target".to_string())
        );
        assert!(!single_lane.lane_local_cargo_target);

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn parallel_claude_has_no_implicit_turn_budget() {
        let args = ParallelArgs {
            action: None,
            apply_receipt_backfill_handoffs: false,
            json: false,
            max_iterations: None,
            max_concurrent_workers: 5,
            cargo_build_jobs: None,
            cargo_target: ParallelCargoTarget::Auto,
            prompt_file: None,
            model: "opus".to_string(),
            reasoning_effort: "xhigh".to_string(),
            branch: None,
            reference_repos: Vec::new(),
            include_siblings: false,
            run_root: None,
            codex_bin: PathBuf::from("codex"),
            claude: true,
            max_turns: None,
            max_retries: 2,
        };

        assert_eq!(effective_parallel_claude_max_turns(&args), None);
    }
}
