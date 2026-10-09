//! Reconcile the CLOWarden configs against what GitHub actually shows.
//!
//! CLOWarden's config is a statement of intent; an export is the state. Where
//! the two disagree, someone holds access nobody declared, or a declaration
//! never took effect. This compares the current revision of every mirrored
//! config against an export and reports each disagreement, so the ones worth
//! looking into are named rather than counted.
//!
//! No API calls: both sides are files already on disk.

use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;

use github_audit_tools::clowarden;
use github_audit_tools::clowarden_drift::{self as drift, Counts, DriftReport, Source, Totals};
use github_audit_tools::members_model::Report;

/// Reconcile the CLOWarden configs against a gh-org-members export.
#[derive(Debug, Parser)]
#[command(name = "clowarden-drift", version, long_about = None)]
struct Args {
    /// The config checkouts mirror-clowarden.zsh made.
    #[arg(long, value_name = "DIR", default_value = "mirror")]
    mirror: PathBuf,

    /// YAML export written by gh-org-members.
    #[arg(
        long,
        value_name = "FILE",
        default_value = "results/lf-decentralized-trust.gh.yaml"
    )]
    people: PathBuf,

    /// Where drift.csv and drift.json go.
    #[arg(long, value_name = "DIR", default_value = "reports")]
    out: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if !args.mirror.is_dir() {
        bail!(
            "no CLOWarden mirror at {}; run ./mirror-clowarden.zsh",
            args.mirror.display()
        );
    }
    let mirror = clowarden::read(&args.mirror, |warning| eprintln!("{warning}"))?;

    let text = std::fs::read_to_string(&args.people)
        .with_context(|| format!("cannot read {}", args.people.display()))?;
    let export: Report = serde_yaml_ng::from_str(&text)
        .with_context(|| format!("{} is not a gh-org-members export", args.people.display()))?;

    let declared = drift::declared(&mirror);
    let actual = drift::actual(&export);
    let orgs = mirror.orgs();
    println!(
        "{} organizations with a config, {} teams declared, {} teams on GitHub",
        orgs.len(),
        declared.len(),
        actual.len()
    );

    let findings = drift::reconcile(&declared, &actual, &orgs);
    let by_kind = Counts::most_common(findings.iter().map(|f| f.kind.as_str().to_string()));
    let by_login = Counts::most_common(findings.iter().map(|f| f.login.clone()));
    let by_org = Counts::most_common(findings.iter().map(|f| f.org.clone()));

    // Organizations the export saw that no config manages: nothing to compare
    // against, which is worth naming so it is not mistaken for agreement.
    let managed: std::collections::BTreeSet<String> =
        orgs.iter().map(|org| org.to_lowercase()).collect();
    let mut unmanaged: Vec<String> = export
        .organizations
        .iter()
        .filter(|org| !managed.contains(&org.to_lowercase()))
        .cloned()
        .collect();
    unmanaged.sort(); // the export orders case-insensitively; this list does not

    let report = DriftReport {
        source: Source {
            mirror: args.mirror.display().to_string(),
            people: args.people.display().to_string(),
            authenticated_as: export.source.authenticated_as.clone(),
            organizations_with_config: orgs.len(),
            organizations_without_config: unmanaged,
            method: "current config revision vs export; no API calls".to_string(),
        },
        totals: Totals {
            findings: findings.len(),
            logins: by_login.len(),
            kinds: Counts(by_kind.sorted()),
        },
        by_org,
        by_login: by_login.clone(),
        findings: findings.clone(),
    };

    std::fs::create_dir_all(&args.out)
        .with_context(|| format!("creating {}", args.out.display()))?;
    let csv_path = args.out.join("drift.csv");
    let json_path = args.out.join("drift.json");
    std::fs::write(&csv_path, drift::csv(&findings))
        .with_context(|| format!("writing {}", csv_path.display()))?;
    let mut json = serde_json::to_string_pretty(&report)?;
    json.push('\n');
    std::fs::write(&json_path, json).with_context(|| format!("writing {}", json_path.display()))?;

    let mut out = std::io::stdout().lock();
    for (kind, count) in &by_kind.0 {
        writeln!(out, "{kind}: {count}")?;
    }
    writeln!(
        out,
        "wrote {} and {} ({} findings across {} logins)",
        csv_path.display(),
        json_path.display(),
        findings.len(),
        by_login.len()
    )?;
    Ok(())
}
