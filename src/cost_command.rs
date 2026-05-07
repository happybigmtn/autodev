use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;

use crate::codex_stream::UsageSidecar;

const SIDECAR_SUFFIX: &str = ".usage.json";

#[derive(Args, Clone)]
pub(crate) struct CostArgs {
    /// Root directory to scan for `*.usage.json` sidecars. Defaults to `.auto/`.
    #[arg(long, value_name = "PATH")]
    pub(crate) root: Option<PathBuf>,
    /// Print every individual sidecar in addition to the per-harness aggregate.
    #[arg(long, default_value_t = false)]
    pub(crate) detail: bool,
}

pub(crate) fn run_cost(args: CostArgs) -> Result<()> {
    let root = args.root.clone().unwrap_or_else(|| PathBuf::from(".auto"));
    if !root.exists() {
        println!("no usage records found under {}", root.display());
        return Ok(());
    }

    let mut sidecars = Vec::new();
    collect_sidecars(&root, &mut sidecars)?;
    if sidecars.is_empty() {
        println!("no usage records found under {}", root.display());
        return Ok(());
    }
    sidecars.sort_by(|a, b| a.path.cmp(&b.path));

    let mut by_harness: BTreeMap<String, AggregateUsage> = BTreeMap::new();
    let mut total = AggregateUsage::default();
    for entry in &sidecars {
        let harness = entry.payload.harness.clone();
        let agg = by_harness.entry(harness).or_default();
        agg.add(&entry.payload);
        total.add(&entry.payload);
    }

    if args.detail {
        println!("# per-invocation");
        for entry in &sidecars {
            let cost = match entry.payload.usage.cost_usd {
                Some(c) => format!("${c:.4}"),
                None => "-".to_string(),
            };
            println!(
                "{harness:>8}  in={input:<10} cached={cached:<10} out={output:<10} cost={cost:<10} {path}",
                harness = entry.payload.harness,
                input = entry.payload.usage.input_tokens,
                cached = entry.payload.usage.cached_input_tokens,
                output = entry.payload.usage.output_tokens,
                cost = cost,
                path = entry.path.display(),
            );
        }
        println!();
    }

    println!("# aggregate by harness");
    println!(
        "{:>8}  {:>9}  {:>10}  {:>10}  {:>10}  {:>10}",
        "harness", "calls", "input", "cached", "output", "cost($)"
    );
    for (harness, agg) in &by_harness {
        println!(
            "{harness:>8}  {calls:>9}  {input:>10}  {cached:>10}  {output:>10}  {cost:>10}",
            harness = harness,
            calls = agg.calls,
            input = agg.input_tokens,
            cached = agg.cached_input_tokens,
            output = agg.output_tokens,
            cost = render_cost(agg.cost_usd),
        );
    }
    println!(
        "{:>8}  {:>9}  {:>10}  {:>10}  {:>10}  {:>10}",
        "TOTAL",
        total.calls,
        total.input_tokens,
        total.cached_input_tokens,
        total.output_tokens,
        render_cost(total.cost_usd),
    );

    Ok(())
}

#[derive(Default)]
struct AggregateUsage {
    calls: u64,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    cost_usd: Option<f64>,
}

impl AggregateUsage {
    fn add(&mut self, payload: &UsageSidecar) {
        self.calls += 1;
        self.input_tokens += payload.usage.input_tokens;
        self.cached_input_tokens += payload.usage.cached_input_tokens;
        self.output_tokens += payload.usage.output_tokens;
        if let Some(cost) = payload.usage.cost_usd {
            *self.cost_usd.get_or_insert(0.0) += cost;
        }
    }
}

fn render_cost(cost: Option<f64>) -> String {
    match cost {
        Some(c) => format!("{c:.4}"),
        None => "-".to_string(),
    }
}

#[derive(Debug)]
struct SidecarEntry {
    path: PathBuf,
    payload: UsageSidecar,
}

fn collect_sidecars(root: &Path, sink: &mut Vec<SidecarEntry>) -> Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to read directory {}", root.display()));
        }
    };
    for entry in entries {
        let entry = entry
            .with_context(|| format!("failed to read directory entry under {}", root.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_sidecars(&path, sink)?;
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.ends_with(SIDECAR_SUFFIX))
            .unwrap_or(false)
        {
            match fs::read_to_string(&path) {
                Ok(body) => match serde_json::from_str::<UsageSidecar>(&body) {
                    Ok(payload) => sink.push(SidecarEntry { path, payload }),
                    Err(err) => {
                        eprintln!(
                            "warning: skipping malformed usage sidecar {}: {err:#}",
                            path.display()
                        );
                    }
                },
                Err(err) => {
                    eprintln!("warning: failed reading {}: {err:#}", path.display());
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("autodev-cost-{label}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn aggregates_sidecars_recursively() {
        let tmp = unique_temp_dir("aggregate");
        let root = tmp.join(".auto");
        let lane_dir = root.join("lanes/lane-1");
        let logs_dir = root.join("logs");
        fs::create_dir_all(&lane_dir).expect("mkdir lane");
        fs::create_dir_all(&logs_dir).expect("mkdir logs");

        let codex = UsageSidecar {
            harness: "codex".to_string(),
            recorded_at: "2026-05-07T00:00:00Z".to_string(),
            usage: crate::codex_stream::UsageSummary {
                input_tokens: 1_000,
                cached_input_tokens: 100,
                output_tokens: 500,
                cost_usd: None,
            },
        };
        let claude = UsageSidecar {
            harness: "claude".to_string(),
            recorded_at: "2026-05-07T00:00:01Z".to_string(),
            usage: crate::codex_stream::UsageSummary {
                input_tokens: 2_000,
                cached_input_tokens: 0,
                output_tokens: 800,
                cost_usd: Some(0.42),
            },
        };

        fs::write(
            lane_dir.join("stdout.log.usage.json"),
            serde_json::to_vec_pretty(&codex).unwrap(),
        )
        .unwrap();
        fs::write(
            logs_dir.join("super.log.usage.json"),
            serde_json::to_vec_pretty(&claude).unwrap(),
        )
        .unwrap();
        fs::write(root.join("not-usage.json"), "{}").unwrap();

        let mut entries = Vec::new();
        collect_sidecars(&root, &mut entries).expect("collect");
        assert_eq!(entries.len(), 2);

        let mut total = AggregateUsage::default();
        for entry in &entries {
            total.add(&entry.payload);
        }
        assert_eq!(total.calls, 2);
        assert_eq!(total.input_tokens, 3_000);
        assert_eq!(total.output_tokens, 1_300);
        assert_eq!(total.cached_input_tokens, 100);
        assert!((total.cost_usd.unwrap() - 0.42).abs() < f64::EPSILON);

        let _ = fs::remove_dir_all(&tmp);
    }
}
