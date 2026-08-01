use super::*;

#[cfg(unix)]
use std::io::Read as _;
#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt as _;

const RUN_ROOT_MARKER_FILE: &str = ".autodev-parallel-root.json";
const RUN_ROOT_LEASE_FILE: &str = ".autodev-parallel-host.lock";
const RUN_ROOT_MARKER_FORMAT: &str = "autodev.parallel-run-root.v1";
const MAX_MARKER_BYTES: u64 = 4096;

#[cfg(unix)]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RunRootMarker {
    format: String,
    root_dev: u64,
    root_ino: u64,
    repo_dev: u64,
    repo_ino: u64,
    lease_dev: u64,
    lease_ino: u64,
    nonce: String,
}

/// Held authority over one `auto parallel` run root.
///
/// The descriptor pins the exact root identity. The marker prevents an
/// operator typo such as `--run-root "$HOME"` from turning an arbitrary
/// existing directory into a destructive purge target.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct ParallelRunRootAuthority {
    path: PathBuf,
    directory: OwnedFd,
    root_dev: u64,
    root_ino: u64,
    repo_dev: u64,
    repo_ino: u64,
    lease: Option<std::fs::File>,
    lease_dev: u64,
    lease_ino: u64,
    marker_nonce: String,
}

#[cfg(not(unix))]
#[derive(Debug)]
pub(crate) struct ParallelRunRootAuthority;

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct SecuredParallelRunRoot {
    path: PathBuf,
    directory: OwnedFd,
    dev: u64,
    ino: u64,
    strict_ancestors: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ParallelRunRootIdentity {
    pub(crate) canonical_path: PathBuf,
    pub(crate) root_dev: u64,
    pub(crate) root_ino: u64,
    pub(crate) marker_nonce: String,
}

#[cfg(unix)]
pub(crate) fn inspect_parallel_run_root_identity(
    run_root: &Path,
) -> Result<ParallelRunRootIdentity> {
    use rustix::fs::{fstat, openat, statat, AtFlags, FileType, Mode, OFlags};
    use rustix::process::geteuid;

    let (directory, root) = open_owned_run_root(run_root)?;
    let marker_fd = openat(
        &directory,
        RUN_ROOT_MARKER_FILE,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .with_context(|| {
        format!(
            "failed to open required parallel authority marker {} without following links",
            run_root.join(RUN_ROOT_MARKER_FILE).display()
        )
    })?;
    let held = fstat(&marker_fd).context("failed to inspect parallel authority marker")?;
    let linked = statat(
        &directory,
        RUN_ROOT_MARKER_FILE,
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .context("failed to revalidate parallel authority marker name")?;
    let mode = held.st_mode & 0o7777;
    if !FileType::from_raw_mode(held.st_mode).is_file()
        || !FileType::from_raw_mode(linked.st_mode).is_file()
        || held.st_uid != geteuid().as_raw()
        || held.st_nlink != 1
        || mode != 0o600
        || held.st_size < 0
        || held.st_size as u64 > MAX_MARKER_BYTES
        || linked.st_dev != held.st_dev
        || linked.st_ino != held.st_ino
    {
        bail!(
            "parallel authority marker {} must be a stable current-uid, single-link, 0600 regular file no larger than {MAX_MARKER_BYTES} bytes",
            run_root.join(RUN_ROOT_MARKER_FILE).display()
        );
    }
    let mut bytes = Vec::with_capacity(held.st_size as usize);
    std::fs::File::from(marker_fd)
        .take(MAX_MARKER_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("failed to read parallel authority marker")?;
    if bytes.len() as u64 > MAX_MARKER_BYTES {
        bail!("parallel authority marker exceeded its size bound");
    }
    let marker: RunRootMarker = serde_json::from_slice(&bytes)
        .context("parallel authority marker is corrupt or unsupported")?;
    if marker.format != RUN_ROOT_MARKER_FORMAT
        || marker.root_dev != root.st_dev
        || marker.root_ino != root.st_ino
        || marker.nonce.len() != 64
        || !marker.nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!(
            "parallel authority marker does not bind selected run root {}",
            run_root.display()
        );
    }
    let (_, reopened) = open_owned_run_root(run_root)?;
    if reopened.st_dev != root.st_dev || reopened.st_ino != root.st_ino {
        bail!(
            "parallel authority root identity changed while inspecting {}",
            run_root.display()
        );
    }
    Ok(ParallelRunRootIdentity {
        canonical_path: std::fs::canonicalize(run_root).with_context(|| {
            format!(
                "failed to resolve selected parallel authority root {}",
                run_root.display()
            )
        })?,
        root_dev: root.st_dev,
        root_ino: root.st_ino,
        marker_nonce: marker.nonce,
    })
}

#[cfg(not(unix))]
pub(crate) fn inspect_parallel_run_root_identity(
    _run_root: &Path,
) -> Result<ParallelRunRootIdentity> {
    bail!("selected parallel run-root identity is only supported on Unix")
}

#[cfg(unix)]
impl ParallelRunRootAuthority {
    pub(crate) fn acquire(repo_root: &Path, run_root: &Path) -> Result<Self> {
        use rustix::fs::{fstat, stat, FileType};

        prepare_parallel_run_root(run_root)?;
        let (directory, root_stat) = open_owned_run_root(run_root)?;
        let repo_stat = stat(repo_root)
            .with_context(|| format!("failed to inspect repository {}", repo_root.display()))?;
        if !FileType::from_raw_mode(repo_stat.st_mode).is_dir() {
            bail!(
                "repository root is not a directory: {}",
                repo_root.display()
            );
        }

        let mut authority = Self {
            path: run_root.to_path_buf(),
            directory,
            root_dev: root_stat.st_dev,
            root_ino: root_stat.st_ino,
            repo_dev: repo_stat.st_dev,
            repo_ino: repo_stat.st_ino,
            lease: None,
            lease_dev: 0,
            lease_ino: 0,
            marker_nonce: String::new(),
        };
        let (lease, lease_stat, marker) = match authority.read_marker()? {
            Some(marker) => {
                authority.validate_marker(&marker)?;
                authority.make_root_private()?;
                let (lease, lease_stat) =
                    authority.acquire_host_lease(Some((marker.lease_dev, marker.lease_ino)))?;
                (lease, lease_stat, marker)
            }
            None => {
                let entries = authority.entry_names()?;
                if entries
                    .iter()
                    .any(|entry| entry != RUN_ROOT_LEASE_FILE)
                {
                    bail!(
                        "parallel run root {} has no Autodev ownership marker and is not empty; \
                         refusing to claim or purge this directory (move its contents or choose a \
                         fresh --run-root)",
                        run_root.display()
                    );
                }
                authority.make_root_private()?;
                let (lease, lease_stat) = authority.acquire_host_lease(None)?;
                authority.create_marker(lease_stat.st_dev, lease_stat.st_ino)?;
                let marker = authority
                    .read_marker()?
                    .context("new run-root ownership marker disappeared")?;
                authority.validate_marker(&marker)?;
                (lease, lease_stat, marker)
            }
        };
        authority.lease_dev = lease_stat.st_dev;
        authority.lease_ino = lease_stat.st_ino;
        authority.marker_nonce = marker.nonce;
        authority.lease = Some(lease);
        authority.revalidate_authority()?;
        let held = fstat(&authority.directory).with_context(|| {
            format!(
                "failed to re-inspect held parallel run root {}",
                run_root.display()
            )
        })?;
        if held.st_dev != authority.root_dev || held.st_ino != authority.root_ino {
            bail!(
                "held parallel run-root identity changed for {}",
                run_root.display()
            );
        }
        Ok(authority)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn make_root_private(&self) -> Result<()> {
        use rustix::fs::{fchmod, fstat, Mode};
        use rustix::process::geteuid;

        fchmod(&self.directory, Mode::from_raw_mode(0o700)).with_context(|| {
            format!(
                "failed to set marker-owned run root {} to mode 0700",
                self.path.display()
            )
        })?;
        let stat = fstat(&self.directory).with_context(|| {
            format!(
                "failed to verify private run root {}",
                self.path.display()
            )
        })?;
        let mode = stat.st_mode & 0o7777;
        if stat.st_uid != geteuid().as_raw() || mode != 0o700 {
            bail!(
                "marker-owned run root {} must be current-uid mode 0700 (uid {}, mode {mode:04o})",
                self.path.display(),
                stat.st_uid
            );
        }
        self.revalidate_path()
    }

    pub(crate) fn duplicate_secured_root(&self) -> Result<SecuredParallelRunRoot> {
        use rustix::fs::fstat;
        use rustix::io::dup;

        self.revalidate_authority()?;
        let directory = dup(&self.directory).with_context(|| {
            format!(
                "failed to duplicate secured parallel run-root descriptor {}",
                self.path.display()
            )
        })?;
        let held = fstat(&directory).context("failed to inspect duplicated run-root descriptor")?;
        if held.st_dev != self.root_dev || held.st_ino != self.root_ino {
            bail!(
                "duplicated parallel run-root identity changed for {}",
                self.path.display()
            );
        }
        Ok(SecuredParallelRunRoot {
            path: self.path.clone(),
            directory,
            dev: self.root_dev,
            ino: self.root_ino,
            strict_ancestors: true,
        })
    }

    pub(crate) fn validate_expected_directory_or_absent(&self, name: &str) -> Result<()> {
        self.validate_expected_entry_or_absent(name, true)
    }

    pub(crate) fn validate_expected_directory_tree_or_absent(&self, name: &str) -> Result<()> {
        use rustix::fs::{fstat, openat, statat, AtFlags, FileType, Mode, OFlags};
        use rustix::io::Errno;

        self.validate_expected_entry_or_absent(name, true)?;
        let entry = match statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(entry) => entry,
            Err(Errno::NOENT) => return Ok(()),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to inspect managed authority subtree {}",
                        self.path.join(name).display()
                    )
                })
            }
        };
        if !FileType::from_raw_mode(entry.st_mode).is_dir() {
            bail!(
                "managed authority subtree {} must be a real no-follow directory",
                self.path.join(name).display()
            );
        }
        let directory = openat(
            &self.directory,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .with_context(|| {
            format!(
                "failed to open managed authority subtree {}",
                self.path.join(name).display()
            )
        })?;
        let held = fstat(&directory).context("failed to inspect managed authority subtree")?;
        if held.st_dev != entry.st_dev
            || held.st_ino != entry.st_ino
            || held.st_dev != self.root_dev
        {
            bail!(
                "managed authority subtree identity changed or crossed filesystems: {}",
                self.path.join(name).display()
            );
        }
        let mut entry_count = 0usize;
        validate_authority_subtree(
            &directory,
            &self.path.join(name),
            self.root_dev,
            0,
            &mut entry_count,
        )?;
        self.validate_expected_entry_or_absent(name, true)?;
        self.revalidate_authority()
    }

    pub(crate) fn validate_expected_regular_file_or_absent(&self, name: &str) -> Result<()> {
        self.validate_expected_entry_or_absent(name, false)
    }

    pub(crate) fn has_valid_regular_file(&self, name: &str) -> Result<bool> {
        use rustix::fs::{statat, AtFlags};
        use rustix::io::Errno;

        self.validate_expected_regular_file_or_absent(name)?;
        match statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => Ok(true),
            Err(Errno::NOENT) => Ok(false),
            Err(err) => Err(err).with_context(|| {
                format!(
                    "failed to inspect managed parallel-run file {}",
                    self.path.join(name).display()
                )
            }),
        }
    }

    pub(crate) fn clear_expected_directory(&self, name: &str) -> Result<u64> {
        use rustix::fs::{fstat, openat, statat, unlinkat, AtFlags, Mode, OFlags};
        use rustix::io::Errno;

        self.validate_expected_directory_or_absent(name)?;
        let entry = match statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(entry) => entry,
            Err(Errno::NOENT) => return Ok(0),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to inspect purge directory {}",
                        self.path.join(name).display()
                    )
                })
            }
        };
        let child = openat(
            &self.directory,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .with_context(|| {
            format!(
                "failed to open purge directory {} without following links",
                self.path.join(name).display()
            )
        })?;
        let held = fstat(&child).context("failed to inspect held purge directory")?;
        if held.st_dev != entry.st_dev
            || held.st_ino != entry.st_ino
            || held.st_dev != self.root_dev
        {
            bail!(
                "purge directory identity changed or crossed filesystems: {}",
                self.path.join(name).display()
            );
        }
        let freed = clear_managed_directory_recursive(
            &child,
            &self.path.join(name),
            self.root_dev,
            0,
        )?;
        let linked = statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .context("failed to revalidate purge directory before removal")?;
        if linked.st_dev != entry.st_dev || linked.st_ino != entry.st_ino {
            bail!(
                "purge directory identity changed before removal: {}",
                self.path.join(name).display()
            );
        }
        unlinkat(&self.directory, name, AtFlags::REMOVEDIR).with_context(|| {
            format!(
                "failed to remove descriptor-relative purge directory {}",
                self.path.join(name).display()
            )
        })?;
        self.revalidate_authority()?;
        Ok(freed)
    }

    pub(crate) fn remove_expected_file(&self, name: &str) -> Result<u64> {
        use rustix::fs::{statat, unlinkat, AtFlags};
        use rustix::io::Errno;

        self.validate_expected_regular_file_or_absent(name)?;
        let entry = match statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(entry) => entry,
            Err(Errno::NOENT) => return Ok(0),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to inspect purge file {}",
                        self.path.join(name).display()
                    )
                })
            }
        };
        let linked = statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .context("failed to revalidate purge file before removal")?;
        if linked.st_dev != entry.st_dev || linked.st_ino != entry.st_ino {
            bail!(
                "purge file identity changed before removal: {}",
                self.path.join(name).display()
            );
        }
        unlinkat(&self.directory, name, AtFlags::empty()).with_context(|| {
            format!(
                "failed to remove descriptor-relative purge file {}",
                self.path.join(name).display()
            )
        })?;
        self.revalidate_authority()?;
        Ok(entry.st_size.max(0) as u64)
    }

    fn validate_expected_entry_or_absent(&self, name: &str, expect_directory: bool) -> Result<()> {
        use rustix::fs::{fstat, openat, statat, AtFlags, FileType, Mode, OFlags};
        use rustix::io::Errno;
        use rustix::process::geteuid;

        self.revalidate_authority()?;
        let entry = match statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(entry) => entry,
            Err(Errno::NOENT) => return Ok(()),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to inspect managed parallel-run entry {}",
                        self.path.join(name).display()
                    )
                })
            }
        };
        let file_type = FileType::from_raw_mode(entry.st_mode);
        let expected_type = if expect_directory {
            "real no-follow directory"
        } else {
            "regular no-follow file"
        };
        let type_matches = if expect_directory {
            file_type.is_dir()
        } else {
            file_type.is_file()
        };
        let mode = entry.st_mode & 0o7777;
        if !type_matches
            || entry.st_uid != geteuid().as_raw()
            || (!expect_directory && entry.st_nlink != 1)
        {
            bail!(
                "managed parallel-run entry {} must be a current-uid {expected_type}{} inside the private run root (uid {}, links {}, mode {mode:04o})",
                self.path.join(name).display(),
                if expect_directory {
                    ""
                } else {
                    " and with one link"
                },
                entry.st_uid,
                entry.st_nlink
            );
        }
        let flags = if expect_directory {
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
        } else {
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC
        };
        let opened = openat(&self.directory, name, flags, Mode::empty()).with_context(|| {
            format!(
                "failed to open managed parallel-run entry {} without following links",
                self.path.join(name).display()
            )
        })?;
        let held = fstat(&opened).with_context(|| {
            format!(
                "failed to inspect managed parallel-run entry {}",
                self.path.join(name).display()
            )
        })?;
        if held.st_dev != entry.st_dev
            || held.st_ino != entry.st_ino
            || (expect_directory && held.st_dev != self.root_dev)
        {
            bail!(
                "managed parallel-run entry identity changed or crossed filesystems: {}",
                self.path.join(name).display()
            );
        }
        self.revalidate_authority()
    }

    fn entry_names(&self) -> Result<Vec<std::ffi::OsString>> {
        use rustix::fs::{openat, Mode, OFlags, RawDir};
        use std::mem::MaybeUninit;

        self.revalidate_path()?;
        let scan_fd = openat(
            &self.directory,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .with_context(|| {
            format!(
                "failed to scan secured parallel run root {}",
                self.path.display()
            )
        })?;
        let mut buffer = [MaybeUninit::<u8>::uninit(); 8192];
        let mut scan = RawDir::new(scan_fd, &mut buffer);
        let mut names = Vec::new();
        while let Some(entry) = scan.next() {
            let entry = entry.with_context(|| {
                format!(
                    "failed reading secured parallel run root {}",
                    self.path.display()
                )
            })?;
            let raw = entry.file_name().to_bytes();
            if raw == b"." || raw == b".." {
                continue;
            }
            names.push(std::ffi::OsStr::from_bytes(raw).to_os_string());
        }
        self.revalidate_path()?;
        Ok(names)
    }

    fn create_marker(&self, lease_dev: u64, lease_ino: u64) -> Result<()> {
        use rustix::fs::{fstat, fsync, openat, FileType, Mode, OFlags};
        use rustix::process::geteuid;

        let marker_fd = openat(
            &self.directory,
            RUN_ROOT_MARKER_FILE,
            OFlags::WRONLY
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .with_context(|| {
            format!(
                "failed to create run-root ownership marker {}",
                self.path.join(RUN_ROOT_MARKER_FILE).display()
            )
        })?;
        let stat = fstat(&marker_fd).context("failed to inspect new run-root ownership marker")?;
        let mode = stat.st_mode & 0o7777;
        if !FileType::from_raw_mode(stat.st_mode).is_file()
            || stat.st_uid != geteuid().as_raw()
            || stat.st_nlink != 1
            || mode != 0o600
        {
            bail!(
                "new run-root ownership marker has unsafe identity (uid {}, links {}, mode {mode:04o})",
                stat.st_uid,
                stat.st_nlink
            );
        }
        let marker = RunRootMarker {
            format: RUN_ROOT_MARKER_FORMAT.to_string(),
            root_dev: self.root_dev,
            root_ino: self.root_ino,
            repo_dev: self.repo_dev,
            repo_ino: self.repo_ino,
            lease_dev,
            lease_ino,
            nonce: random_nonce()?,
        };
        let bytes =
            serde_json::to_vec(&marker).context("failed to serialize run-root ownership marker")?;
        let mut marker_file = std::fs::File::from(marker_fd);
        marker_file
            .write_all(&bytes)
            .context("failed to write run-root ownership marker")?;
        marker_file
            .sync_all()
            .context("failed to sync run-root ownership marker")?;
        fsync(&self.directory).context("failed to sync parallel run-root directory")?;
        self.revalidate_path()
    }

    fn read_marker(&self) -> Result<Option<RunRootMarker>> {
        use rustix::fs::{fstat, openat, statat, AtFlags, FileType, Mode, OFlags};
        use rustix::io::Errno;
        use rustix::process::geteuid;

        self.revalidate_path()?;
        let marker_fd = match openat(
            &self.directory,
            RUN_ROOT_MARKER_FILE,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to open run-root ownership marker {} without following links",
                        self.path.join(RUN_ROOT_MARKER_FILE).display()
                    )
                })
            }
        };
        let held = fstat(&marker_fd).context("failed to inspect held run-root ownership marker")?;
        let mode = held.st_mode & 0o7777;
        if !FileType::from_raw_mode(held.st_mode).is_file()
            || held.st_uid != geteuid().as_raw()
            || held.st_nlink != 1
            || mode != 0o600
            || held.st_size < 0
            || held.st_size as u64 > MAX_MARKER_BYTES
        {
            bail!(
                "run-root ownership marker {} must be a current-uid, single-link, 0600 regular file no larger than {MAX_MARKER_BYTES} bytes (uid {}, links {}, mode {mode:04o}, size {})",
                self.path.join(RUN_ROOT_MARKER_FILE).display(),
                held.st_uid,
                held.st_nlink,
                held.st_size
            );
        }
        let linked = statat(
            &self.directory,
            RUN_ROOT_MARKER_FILE,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .context("failed to revalidate run-root ownership marker name")?;
        if linked.st_dev != held.st_dev
            || linked.st_ino != held.st_ino
            || !FileType::from_raw_mode(linked.st_mode).is_file()
        {
            bail!(
                "run-root ownership marker identity changed under {}",
                self.path.display()
            );
        }
        let mut bytes = Vec::with_capacity(held.st_size as usize);
        std::fs::File::from(marker_fd)
            .take(MAX_MARKER_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("failed to read run-root ownership marker")?;
        if bytes.len() as u64 > MAX_MARKER_BYTES {
            bail!("run-root ownership marker exceeded its size bound");
        }
        let marker = serde_json::from_slice(&bytes)
            .context("run-root ownership marker is corrupt or unsupported")?;
        self.revalidate_path()?;
        Ok(Some(marker))
    }

    fn acquire_host_lease(
        &self,
        expected_identity: Option<(u64, u64)>,
    ) -> Result<(std::fs::File, rustix::fs::Stat)> {
        use rustix::fs::{
            flock, fstat, openat, statat, AtFlags, FileType, FlockOperation, Mode, OFlags,
        };
        use rustix::io::Errno;
        use rustix::process::geteuid;

        self.revalidate_path()?;
        let flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let lease_fd = match openat(
            &self.directory,
            RUN_ROOT_LEASE_FILE,
            flags,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) if expected_identity.is_none() => openat(
                &self.directory,
                RUN_ROOT_LEASE_FILE,
                flags | OFlags::CREATE | OFlags::EXCL,
                Mode::from_raw_mode(0o600),
            )
            .or_else(|err| {
                if err == Errno::EXIST {
                    openat(
                        &self.directory,
                        RUN_ROOT_LEASE_FILE,
                        flags,
                        Mode::empty(),
                    )
                } else {
                    Err(err)
                }
            })
            .with_context(|| {
                format!(
                    "failed to create run-root host lease {}",
                    self.path.join(RUN_ROOT_LEASE_FILE).display()
                )
            })?,
            Err(Errno::NOENT) => {
                bail!(
                    "run-root host lease required by ownership marker is missing: {}",
                    self.path.join(RUN_ROOT_LEASE_FILE).display()
                )
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to open run-root host lease {} without following links",
                        self.path.join(RUN_ROOT_LEASE_FILE).display()
                    )
                })
            }
        };
        let held = fstat(&lease_fd).context("failed to inspect held run-root host lease")?;
        let mode = held.st_mode & 0o7777;
        if !FileType::from_raw_mode(held.st_mode).is_file()
            || held.st_uid != geteuid().as_raw()
            || held.st_nlink != 1
            || mode != 0o600
        {
            bail!(
                "run-root host lease {} must be a current-uid, single-link, 0600 regular file (uid {}, links {}, mode {mode:04o})",
                self.path.join(RUN_ROOT_LEASE_FILE).display(),
                held.st_uid,
                held.st_nlink
            );
        }
        if expected_identity
            .is_some_and(|(dev, ino)| held.st_dev != dev || held.st_ino != ino)
        {
            bail!(
                "run-root host lease identity does not match ownership marker under {}",
                self.path.display()
            );
        }
        flock(&lease_fd, FlockOperation::NonBlockingLockExclusive).with_context(|| {
            format!(
                "parallel run root {} already has an active host lease",
                self.path.display()
            )
        })?;
        let linked = statat(
            &self.directory,
            RUN_ROOT_LEASE_FILE,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .context("failed to revalidate run-root host lease name")?;
        if linked.st_dev != held.st_dev
            || linked.st_ino != held.st_ino
            || !FileType::from_raw_mode(linked.st_mode).is_file()
        {
            bail!(
                "run-root host lease identity changed under {}",
                self.path.display()
            );
        }
        self.revalidate_path()?;
        Ok((std::fs::File::from(lease_fd), held))
    }

    fn validate_marker(&self, marker: &RunRootMarker) -> Result<()> {
        if marker.format != RUN_ROOT_MARKER_FORMAT
            || marker.root_dev != self.root_dev
            || marker.root_ino != self.root_ino
            || marker.repo_dev != self.repo_dev
            || marker.repo_ino != self.repo_ino
            || marker.nonce.len() != 64
            || !marker.nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            bail!(
                "run-root ownership marker does not authorize repository {} and root identity {}",
                self.repo_dev,
                self.path.display()
            );
        }
        Ok(())
    }

    pub(crate) fn revalidate_authority(&self) -> Result<()> {
        use rustix::fs::{fstat, statat, AtFlags, FileType};

        self.revalidate_path()?;
        let marker = self
            .read_marker()?
            .context("run-root ownership marker disappeared while host was active")?;
        self.validate_marker(&marker)?;
        if marker.nonce != self.marker_nonce
            || marker.lease_dev != self.lease_dev
            || marker.lease_ino != self.lease_ino
        {
            bail!(
                "run-root ownership marker identity changed while host was active: {}",
                self.path.display()
            );
        }
        let lease = self
            .lease
            .as_ref()
            .context("parallel run-root authority lost its host lease")?;
        let held = fstat(lease).context("failed to inspect held run-root host lease")?;
        let linked = statat(
            &self.directory,
            RUN_ROOT_LEASE_FILE,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .context("failed to inspect linked run-root host lease")?;
        if held.st_dev != self.lease_dev
            || held.st_ino != self.lease_ino
            || linked.st_dev != self.lease_dev
            || linked.st_ino != self.lease_ino
            || !FileType::from_raw_mode(held.st_mode).is_file()
            || !FileType::from_raw_mode(linked.st_mode).is_file()
        {
            bail!(
                "run-root host lease identity changed while host was active: {}",
                self.path.display()
            );
        }
        self.revalidate_path()
    }

    pub(crate) fn revalidate_path(&self) -> Result<()> {
        use rustix::fs::fstat;

        let held = fstat(&self.directory).with_context(|| {
            format!(
                "failed to inspect held parallel run root {}",
                self.path.display()
            )
        })?;
        if held.st_dev != self.root_dev || held.st_ino != self.root_ino {
            bail!(
                "held parallel run-root identity changed for {}",
                self.path.display()
            );
        }
        let (_, reopened) = open_owned_run_root(&self.path)?;
        if reopened.st_dev != self.root_dev || reopened.st_ino != self.root_ino {
            bail!(
                "parallel run-root path was replaced after authority acquisition: {}",
                self.path.display()
            );
        }
        Ok(())
    }
}

#[cfg(unix)]
impl SecuredParallelRunRoot {
    #[cfg(test)]
    pub(crate) fn open_unleased(path: &Path) -> Result<Self> {
        use rustix::fs::{fstat, open, FileType, Mode, OFlags};
        use rustix::process::geteuid;

        let directory = open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .with_context(|| {
            format!(
                "failed to open test parallel run root {} without following links",
                path.display()
            )
        })?;
        let stat = fstat(&directory).with_context(|| {
            format!("failed to inspect test parallel run root {}", path.display())
        })?;
        if !FileType::from_raw_mode(stat.st_mode).is_dir()
            || stat.st_uid != geteuid().as_raw()
        {
            bail!(
                "test parallel run root {} must be a current-uid directory",
                path.display()
            );
        }
        let secured = Self {
            path: path.to_path_buf(),
            directory,
            dev: stat.st_dev,
            ino: stat.st_ino,
            strict_ancestors: false,
        };
        secured.revalidate()?;
        Ok(secured)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn directory(&self) -> &OwnedFd {
        &self.directory
    }

    pub(crate) fn revalidate(&self) -> Result<()> {
        use rustix::fs::fstat;

        let held = fstat(&self.directory).with_context(|| {
            format!(
                "failed to inspect held parallel run-root descriptor {}",
                self.path.display()
            )
        })?;
        if held.st_dev != self.dev || held.st_ino != self.ino {
            bail!(
                "held parallel run-root descriptor identity changed for {}",
                self.path.display()
            );
        }
        let reopened = if self.strict_ancestors {
            open_owned_run_root(&self.path)?.1
        } else {
            use rustix::fs::{fstat, open, Mode, OFlags};
            let directory = open(
                &self.path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .with_context(|| {
                format!(
                    "failed to reopen test parallel run root {} without following links",
                    self.path.display()
                )
            })?;
            fstat(directory).with_context(|| {
                format!(
                    "failed to inspect reopened test parallel run root {}",
                    self.path.display()
                )
            })?
        };
        if reopened.st_dev != self.dev || reopened.st_ino != self.ino {
            bail!(
                "parallel run-root path identity changed: {}",
                self.path.display()
            );
        }
        Ok(())
    }
}

#[cfg(not(unix))]
impl ParallelRunRootAuthority {
    pub(crate) fn acquire(_repo_root: &Path, run_root: &Path) -> Result<Self> {
        bail!(
            "parallel run-root authority for {} requires Unix no-follow descriptors and advisory locks",
            run_root.display()
        )
    }
}

#[cfg(unix)]
fn open_owned_run_root(path: &Path) -> Result<(OwnedFd, rustix::fs::Stat)> {
    use rustix::fs::{fstat, open, openat, FileType, Mode, OFlags};
    use rustix::process::geteuid;
    use std::path::Component;

    const GROUP_OR_OTHER_WRITE: u32 = 0o022;
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
    let mut current =
        open("/", flags, Mode::empty()).context("failed to open filesystem root")?;
    let mut traversed = PathBuf::from("/");
    for (index, name) in names.iter().enumerate() {
        let current_stat = fstat(&current).with_context(|| {
            format!(
                "failed to inspect parallel-run ancestor {}",
                traversed.display()
            )
        })?;
        let mode = current_stat.st_mode & 0o7777;
        let trusted_sticky = current_stat.st_uid == 0 && mode & STICKY != 0;
        if current_stat.st_uid != 0 && current_stat.st_uid != euid {
            bail!(
                "parallel-run ancestor {} is owned by untrusted uid {}",
                traversed.display(),
                current_stat.st_uid
            );
        }
        if mode & GROUP_OR_OTHER_WRITE != 0 && !trusted_sticky {
            bail!(
                "parallel-run ancestor {} is group/other-writable (mode {mode:04o})",
                traversed.display()
            );
        }
        let child = openat(&current, name, flags, Mode::empty()).with_context(|| {
            format!(
                "refusing parallel-run component {} that is not a no-follow directory",
                traversed.join(name).display()
            )
        })?;
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
            return Ok((child, stat));
        }
        current = child;
    }
    unreachable!("absolute non-root path has at least one component")
}

#[cfg(unix)]
fn random_nonce() -> Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .context("failed to open operating-system random source")?
        .read_exact(&mut bytes)
        .context("failed to read operating-system random source")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(unix)]
fn validate_authority_subtree(
    directory: &OwnedFd,
    display_path: &Path,
    expected_dev: u64,
    depth: usize,
    entry_count: &mut usize,
) -> Result<()> {
    use rustix::fs::{fstat, openat, statat, AtFlags, FileType, Mode, OFlags, RawDir};
    use rustix::process::geteuid;
    use std::mem::MaybeUninit;

    const MAX_AUTHORITY_DEPTH: usize = 32;
    const MAX_AUTHORITY_ENTRIES: usize = 100_000;

    if depth > MAX_AUTHORITY_DEPTH {
        bail!(
            "managed authority subtree exceeded depth bound {MAX_AUTHORITY_DEPTH}: {}",
            display_path.display()
        );
    }
    let scan_fd = openat(
        directory,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .with_context(|| {
        format!(
            "failed to scan managed authority subtree {}",
            display_path.display()
        )
    })?;
    let mut buffer = [MaybeUninit::<u8>::uninit(); 8192];
    let mut scan = RawDir::new(scan_fd, &mut buffer);
    while let Some(raw_entry) = scan.next() {
        let raw_entry = raw_entry.with_context(|| {
            format!(
                "failed reading managed authority subtree {}",
                display_path.display()
            )
        })?;
        let raw_name = raw_entry.file_name().to_bytes();
        if raw_name == b"." || raw_name == b".." {
            continue;
        }
        *entry_count = entry_count.saturating_add(1);
        if *entry_count > MAX_AUTHORITY_ENTRIES {
            bail!(
                "managed authority subtree exceeded entry bound {MAX_AUTHORITY_ENTRIES}: {}",
                display_path.display()
            );
        }
        let name = std::ffi::OsStr::from_bytes(raw_name);
        let path = display_path.join(name);
        let entry = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .with_context(|| format!("failed to inspect authority entry {}", path.display()))?;
        let mode = entry.st_mode & 0o7777;
        let file_type = FileType::from_raw_mode(entry.st_mode);
        if entry.st_uid != geteuid().as_raw() || entry.st_dev != expected_dev {
            bail!(
                "managed authority entry {} must stay on the private run-root filesystem and be current-uid (uid {}, mode {mode:04o})",
                path.display(),
                entry.st_uid
            );
        }
        if file_type.is_dir() {
            let child = openat(
                directory,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .with_context(|| {
                format!(
                    "failed to open authority directory {} without following links",
                    path.display()
                )
            })?;
            let held = fstat(&child)
                .with_context(|| format!("failed to inspect authority directory {}", path.display()))?;
            if held.st_dev != entry.st_dev || held.st_ino != entry.st_ino {
                bail!("authority directory identity changed: {}", path.display());
            }
            validate_authority_subtree(
                &child,
                &path,
                expected_dev,
                depth + 1,
                entry_count,
            )?;
            let linked = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).with_context(|| {
                format!("failed to revalidate authority directory {}", path.display())
            })?;
            if linked.st_dev != entry.st_dev || linked.st_ino != entry.st_ino {
                bail!("authority directory identity changed: {}", path.display());
            }
        } else if file_type.is_file() {
            if entry.st_nlink != 1 {
                bail!(
                    "managed authority file {} must have exactly one link (found {})",
                    path.display(),
                    entry.st_nlink
                );
            }
            let file = openat(
                directory,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .with_context(|| {
                format!(
                    "failed to open authority file {} without following links",
                    path.display()
                )
            })?;
            let held = fstat(&file)
                .with_context(|| format!("failed to inspect authority file {}", path.display()))?;
            if held.st_dev != entry.st_dev || held.st_ino != entry.st_ino {
                bail!("authority file identity changed: {}", path.display());
            }
        } else {
            bail!(
                "managed authority entry {} must be a no-follow directory or regular file",
                path.display()
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
fn clear_managed_directory_recursive(
    directory: &OwnedFd,
    display_path: &Path,
    expected_dev: u64,
    depth: usize,
) -> Result<u64> {
    use rustix::fs::{
        fstat, openat, statat, unlinkat, AtFlags, FileType, Mode, OFlags, RawDir,
    };
    use rustix::process::geteuid;
    use std::mem::MaybeUninit;

    const MAX_PURGE_DEPTH: usize = 128;

    if depth > MAX_PURGE_DEPTH {
        bail!(
            "managed purge tree exceeded depth bound {MAX_PURGE_DEPTH}: {}",
            display_path.display()
        );
    }
    let scan_fd = openat(
        directory,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .with_context(|| {
        format!(
            "failed to scan descriptor-relative purge tree {}",
            display_path.display()
        )
    })?;
    let mut buffer = [MaybeUninit::<u8>::uninit(); 8192];
    let mut scan = RawDir::new(scan_fd, &mut buffer);
    let mut entries = Vec::new();
    while let Some(raw_entry) = scan.next() {
        let raw_entry = raw_entry.with_context(|| {
            format!(
                "failed reading descriptor-relative purge tree {}",
                display_path.display()
            )
        })?;
        let raw_name = raw_entry.file_name().to_bytes();
        if raw_name == b"." || raw_name == b".." {
            continue;
        }
        let name = std::ffi::OsStr::from_bytes(raw_name).to_os_string();
        let stat = statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW).with_context(|| {
            format!(
                "failed to inspect purge entry {}",
                display_path.join(&name).display()
            )
        })?;
        entries.push((name, stat));
    }

    let mut freed = 0u64;
    for (name, entry) in entries {
        let path = display_path.join(&name);
        let file_type = FileType::from_raw_mode(entry.st_mode);
        let mode = entry.st_mode & 0o7777;
        if entry.st_uid != geteuid().as_raw() {
            bail!(
                "refusing purge entry {} owned by uid {}",
                path.display(),
                entry.st_uid
            );
        }
        if file_type.is_dir() {
            if entry.st_dev != expected_dev {
                bail!(
                    "purge directory {} must stay on the private run-root filesystem (mode {mode:04o})",
                    path.display()
                );
            }
            let child = openat(
                directory,
                &name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .with_context(|| {
                format!(
                    "failed to open purge directory {} without following links",
                    path.display()
                )
            })?;
            let held = fstat(&child)
                .with_context(|| format!("failed to inspect purge directory {}", path.display()))?;
            if held.st_dev != entry.st_dev || held.st_ino != entry.st_ino {
                bail!("purge directory identity changed: {}", path.display());
            }
            freed = freed.saturating_add(clear_managed_directory_recursive(
                &child,
                &path,
                expected_dev,
                depth + 1,
            )?);
            let linked =
                statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW).with_context(|| {
                    format!("failed to revalidate purge directory {}", path.display())
                })?;
            if linked.st_dev != entry.st_dev || linked.st_ino != entry.st_ino {
                bail!("purge directory identity changed: {}", path.display());
            }
            unlinkat(directory, &name, AtFlags::REMOVEDIR).with_context(|| {
                format!("failed to remove purge directory {}", path.display())
            })?;
        } else if file_type.is_file() {
            if entry.st_dev != expected_dev || entry.st_nlink != 1 {
                bail!(
                    "purge file {} must stay on the private run-root filesystem and have one link (links {}, mode {mode:04o})",
                    path.display(),
                    entry.st_nlink
                );
            }
            let linked =
                statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW).with_context(|| {
                    format!("failed to revalidate purge file {}", path.display())
                })?;
            if linked.st_dev != entry.st_dev || linked.st_ino != entry.st_ino {
                bail!("purge file identity changed: {}", path.display());
            }
            unlinkat(directory, &name, AtFlags::empty())
                .with_context(|| format!("failed to remove purge file {}", path.display()))?;
            freed = freed.saturating_add(entry.st_size.max(0) as u64);
        } else if file_type.is_symlink() {
            let linked =
                statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW).with_context(|| {
                    format!("failed to revalidate purge symlink {}", path.display())
                })?;
            if linked.st_dev != entry.st_dev
                || linked.st_ino != entry.st_ino
                || !FileType::from_raw_mode(linked.st_mode).is_symlink()
            {
                bail!("purge symlink identity changed: {}", path.display());
            }
            unlinkat(directory, &name, AtFlags::empty())
                .with_context(|| format!("failed to unlink purge symlink {}", path.display()))?;
            freed = freed.saturating_add(entry.st_size.max(0) as u64);
        } else {
            bail!(
                "refusing unsupported special file in purge tree: {}",
                path.display()
            );
        }
    }
    Ok(freed)
}
