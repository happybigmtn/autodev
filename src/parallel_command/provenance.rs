use super::*;

use serde::{Deserialize, Serialize};

const HOST_PROVENANCE_FILE: &str = ".host-provenance.json";
const HOST_PROVENANCE_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct BinaryProvenance {
    pub(crate) version: String,
    pub(crate) commit: String,
    pub(crate) dirty: String,
    pub(crate) profile: String,
    pub(crate) executable: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct HostProvenanceMarker {
    version: u32,
    run_id: String,
    pid: u32,
    binary: BinaryProvenance,
}

pub(crate) fn current_binary_provenance() -> BinaryProvenance {
    BinaryProvenance {
        version: env!("CARGO_PKG_VERSION").to_string(),
        commit: env!("AUTODEV_GIT_SHA").to_string(),
        dirty: env!("AUTODEV_GIT_DIRTY").to_string(),
        profile: env!("AUTODEV_BUILD_PROFILE").to_string(),
        executable: crate::util::current_binary_path(),
    }
}

pub(crate) fn persist_parallel_host_provenance(run_root: &Path, run_id: &str) -> Result<()> {
    let binary = current_binary_provenance();
    let marker = HostProvenanceMarker {
        version: HOST_PROVENANCE_VERSION,
        run_id: run_id.to_string(),
        pid: std::process::id(),
        binary,
    };
    let bytes = serde_json::to_vec_pretty(&marker).context("serialize parallel host provenance")?;
    atomic_write(&run_root.join(HOST_PROVENANCE_FILE), &bytes)
        .context("persist parallel host provenance")
}

pub(crate) fn parallel_host_binary_provenance(
    run_root: &Path,
    detected_host_pids: &BTreeSet<u32>,
) -> Option<BinaryProvenance> {
    if detected_host_pids.is_empty() {
        return None;
    }
    let current_run_id = current_parallel_run_id(run_root)?;
    let bytes = fs::read(run_root.join(HOST_PROVENANCE_FILE)).ok()?;
    let marker: HostProvenanceMarker = serde_json::from_slice(&bytes).ok()?;
    if marker.version != HOST_PROVENANCE_VERSION
        || marker.run_id != current_run_id
        || !detected_host_pids.contains(&marker.pid)
        || !valid_build_commit(&marker.binary.commit)
        || !matches!(marker.binary.dirty.as_str(), "clean" | "dirty" | "unknown")
        || !matches!(
            marker.binary.profile.as_str(),
            "debug" | "release" | "unknown"
        )
        || marker.binary.version.trim().is_empty()
        || marker.binary.executable.trim().is_empty()
    {
        return None;
    }
    Some(marker.binary)
}

pub(crate) fn binary_revision_match(
    status_binary: &BinaryProvenance,
    host_binary: Option<&BinaryProvenance>,
) -> Option<bool> {
    let host_binary = host_binary?;
    if status_binary.dirty != "clean"
        || host_binary.dirty != "clean"
        || !valid_build_commit(&status_binary.commit)
        || !valid_build_commit(&host_binary.commit)
    {
        return None;
    }
    Some(status_binary.commit == host_binary.commit)
}

fn valid_build_commit(commit: &str) -> bool {
    (7..=40).contains(&commit.len()) && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture_binary(commit: &str) -> BinaryProvenance {
        BinaryProvenance {
            version: "0.2.0".to_string(),
            commit: commit.to_string(),
            dirty: "clean".to_string(),
            profile: "release".to_string(),
            executable: "/opt/autodev/auto".to_string(),
        }
    }

    fn temp_run_root(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-provenance-{label}-{stamp}"))
    }

    fn write_marker(run_root: &Path, run_id: &str, commit: &str) {
        fs::create_dir_all(run_root).expect("create run root");
        fs::write(run_root.join(CURRENT_RUN_ID_FILE), run_id).expect("write run id");
        let marker = HostProvenanceMarker {
            version: HOST_PROVENANCE_VERSION,
            run_id: run_id.to_string(),
            pid: 42,
            binary: fixture_binary(commit),
        };
        fs::write(
            run_root.join(HOST_PROVENANCE_FILE),
            serde_json::to_vec_pretty(&marker).expect("serialize marker"),
        )
        .expect("write marker");
    }

    #[test]
    fn older_host_without_provenance_is_explicitly_unknown() {
        let run_root = temp_run_root("older-host");
        fs::create_dir_all(&run_root).expect("create run root");
        fs::write(run_root.join(CURRENT_RUN_ID_FILE), "older-run").expect("write run id");
        let status_binary = fixture_binary("bbbbbbb");

        let host_binary = parallel_host_binary_provenance(&run_root, &BTreeSet::from([42]));

        assert_eq!(host_binary, None);
        assert_eq!(
            binary_revision_match(&status_binary, host_binary.as_ref()),
            None
        );
        fs::remove_dir_all(run_root).expect("cleanup run root");
    }

    #[test]
    fn matching_and_mismatching_host_revisions_are_machine_readable() {
        let run_root = temp_run_root("revision-match");
        let status_binary = fixture_binary("bbbbbbb");
        write_marker(&run_root, "current-run", "bbbbbbb");
        let matching = parallel_host_binary_provenance(&run_root, &BTreeSet::from([42]));
        assert_eq!(
            binary_revision_match(&status_binary, matching.as_ref()),
            Some(true)
        );

        write_marker(&run_root, "current-run", "aaaaaaa");
        let mismatching = parallel_host_binary_provenance(&run_root, &BTreeSet::from([42]));
        assert_eq!(
            binary_revision_match(&status_binary, mismatching.as_ref()),
            Some(false)
        );
        fs::remove_dir_all(run_root).expect("cleanup run root");
    }

    #[test]
    fn dirty_builds_never_infer_a_revision_match() {
        let clean = fixture_binary("bbbbbbb");
        let mut dirty = clean.clone();
        dirty.dirty = "dirty".to_string();

        assert_eq!(binary_revision_match(&dirty, Some(&clean)), None);
        assert_eq!(binary_revision_match(&clean, Some(&dirty)), None);
    }

    #[test]
    fn marker_pid_must_identify_the_detected_live_host() {
        let run_root = temp_run_root("different-host-pid");
        write_marker(&run_root, "current-run", "bbbbbbb");

        let host_binary = parallel_host_binary_provenance(&run_root, &BTreeSet::from([99]));

        assert_eq!(host_binary, None);
        fs::remove_dir_all(run_root).expect("cleanup run root");
    }

    #[test]
    fn stale_or_malformed_host_marker_never_infers_a_match() {
        let run_root = temp_run_root("stale-marker");
        write_marker(&run_root, "old-run", "bbbbbbb");
        fs::write(run_root.join(CURRENT_RUN_ID_FILE), "new-run").expect("replace run id");
        let status_binary = fixture_binary("bbbbbbb");

        let host_binary = parallel_host_binary_provenance(&run_root, &BTreeSet::from([42]));

        assert_eq!(host_binary, None);
        assert_eq!(
            binary_revision_match(&status_binary, host_binary.as_ref()),
            None
        );
        fs::remove_dir_all(run_root).expect("cleanup run root");
    }
}
