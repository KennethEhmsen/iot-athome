//! `iotctl rule …` — add, list, delete, test (M3 W2.3).
//!
//! The CLI surface for the automation engine. Rules live on disk as
//! YAML files in `<rules_dir>/<id>.yaml`; `add` validates + copies a
//! file in, `list` enumerates installed rules, `delete` removes one,
//! and `test` dry-runs a rule against a synthetic payload without
//! touching the bus — the fastest way to iterate on a rule's `when`
//! expression.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _, Result};
use clap::Subcommand;

use iot_automation::expr;
use iot_automation::rule::{RawAction, Rule};

/// Default rules directory. Kept in sync with the automation engine's
/// own default — both services read the same path.
pub const DEFAULT_RULES_DIR: &str = "/var/lib/iotathome/rules";

#[derive(Debug, Subcommand)]
pub enum RuleCmd {
    /// Validate a rule YAML file and copy it into the rules directory.
    Add {
        /// Path to the source YAML file.
        path: PathBuf,
        #[arg(long, env = "IOT_RULES_DIR", default_value = DEFAULT_RULES_DIR)]
        rules_dir: PathBuf,
        /// Replace an already-installed rule with the same id.
        #[arg(long)]
        force: bool,
    },
    /// List installed rules with their triggers + action counts.
    List {
        #[arg(long, env = "IOT_RULES_DIR", default_value = DEFAULT_RULES_DIR)]
        rules_dir: PathBuf,
    },
    /// Remove an installed rule by id.
    Delete {
        /// Rule id (matches `id` in the YAML).
        id: String,
        #[arg(long, env = "IOT_RULES_DIR", default_value = DEFAULT_RULES_DIR)]
        rules_dir: PathBuf,
    },
    /// Dry-run a rule against a synthetic `(subject, payload)` without
    /// touching the bus. Prints whether the `when` expression
    /// evaluates true and what actions the rule would have emitted.
    Test {
        /// Path to the rule YAML file.
        rule: PathBuf,
        /// Trigger subject to simulate. Defaults to the rule's first
        /// declared trigger.
        #[arg(long)]
        subject: Option<String>,
        /// Inline JSON payload. Accepts either raw JSON
        /// (`--payload '{"value":25}'`) or `@path/to/file.json`.
        #[arg(long, default_value = "{}")]
        payload: String,
    },
}

pub fn run(cmd: &RuleCmd) -> Result<()> {
    match cmd {
        RuleCmd::Add {
            path,
            rules_dir,
            force,
        } => add(path, rules_dir, *force),
        RuleCmd::List { rules_dir } => list(rules_dir),
        RuleCmd::Delete { id, rules_dir } => delete(id, rules_dir),
        RuleCmd::Test {
            rule,
            subject,
            payload,
        } => test(rule, subject.as_deref(), payload),
    }
}

// ------------------------------------------------------------- add

fn add(src: &Path, rules_dir: &Path, force: bool) -> Result<()> {
    // Validate by *compiling* via the same parser the engine uses —
    // catches expression errors + structural violations before the
    // file lands in the live rules directory.
    let rule =
        Rule::from_file(src).with_context(|| format!("compile rule from {}", src.display()))?;
    let dest = rules_dir.join(format!("{}.yaml", rule.id));
    if dest.exists() && !force {
        bail!(
            "{} already installed — use --force to replace, or `iotctl rule delete {}` first",
            dest.display(),
            rule.id
        );
    }
    fs::create_dir_all(rules_dir).with_context(|| format!("mkdir -p {}", rules_dir.display()))?;
    fs::copy(src, &dest).with_context(|| format!("copy {} → {}", src.display(), dest.display()))?;
    println!(
        "installed rule {} ({} triggers, {} actions) → {}",
        rule.id,
        rule.triggers.len(),
        rule.actions.len(),
        dest.display()
    );
    Ok(())
}

// ------------------------------------------------------------- list

fn list(rules_dir: &Path) -> Result<()> {
    if !rules_dir.exists() {
        println!("(no rules installed at {})", rules_dir.display());
        return Ok(());
    }
    let mut rules: Vec<Rule> = Vec::new();
    for entry in
        fs::read_dir(rules_dir).with_context(|| format!("read_dir {}", rules_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
            continue;
        }
        match Rule::from_file(&path) {
            Ok(r) => rules.push(r),
            Err(e) => tracing::warn!(
                path = %path.display(),
                error = %format!("{e:#}"),
                "skipping unreadable rule"
            ),
        }
    }
    if rules.is_empty() {
        println!("(no rules installed at {})", rules_dir.display());
        return Ok(());
    }
    rules.sort_by(|a, b| a.id.cmp(&b.id));
    println!("{:<36} {:<60} {:<8}", "ID", "DESCRIPTION", "ACTIONS");
    for r in &rules {
        let description = if r.description.is_empty() {
            "-"
        } else {
            r.description.as_str()
        };
        let description = truncate(description, 60);
        println!("{:<36} {:<60} {:<8}", r.id, description, r.actions.len());
        // Continuation line so the main row stays grep-friendly.
        for t in &r.triggers {
            println!("    trigger: {t}");
        }
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

// ------------------------------------------------------------- delete

fn delete(id: &str, rules_dir: &Path) -> Result<()> {
    let path = rules_dir.join(format!("{id}.yaml"));
    if !path.is_file() {
        bail!("{} is not installed under {}", id, rules_dir.display());
    }
    fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    println!("deleted rule {id} ({})", path.display());
    Ok(())
}

// ------------------------------------------------------------- test

fn test(rule_path: &Path, subject_override: Option<&str>, payload: &str) -> Result<()> {
    let rule = Rule::from_file(rule_path)
        .with_context(|| format!("compile rule {}", rule_path.display()))?;
    let subject = subject_override
        .map(ToOwned::to_owned)
        .or_else(|| rule.triggers.first().cloned())
        .ok_or_else(|| anyhow!("rule has no triggers and no --subject provided"))?;

    if !rule.triggers_on(&subject) {
        println!(
            "subject `{subject}` does NOT match any of the rule's triggers; \
             the rule would not fire"
        );
        return Ok(());
    }

    let payload_json = read_payload(payload)?;
    println!("rule    {}", rule.id);
    println!("subject {subject}");
    println!("payload {payload_json}");

    match expr::eval_bool(&rule.when, &payload_json) {
        Ok(true) => {
            println!("when    → true; rule would fire:");
            for action in &rule.actions {
                match action {
                    RawAction::Publish {
                        subject: s,
                        iot_type,
                        payload: p,
                    } => {
                        println!(
                            "  publish → {s} (iot-type={iot_type}, payload={})",
                            serde_json::to_string(p).unwrap_or_default()
                        );
                    }
                    RawAction::Log { level, message } => {
                        println!("  log     → {level}: {message}");
                    }
                }
            }
        }
        Ok(false) => println!("when    → false; rule would NOT fire"),
        Err(e) => println!("when    → evaluation error: {e}"),
    }
    Ok(())
}

/// `payload` is either raw JSON (`'{"foo":1}'`) or `@path/to/file.json`.
fn read_payload(arg: &str) -> Result<serde_json::Value> {
    let raw = if let Some(path) = arg.strip_prefix('@') {
        fs::read_to_string(path).with_context(|| format!("read {path}"))?
    } else {
        arg.to_owned()
    };
    serde_json::from_str(&raw).with_context(|| "parse payload as JSON")
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    const DEMO_RULE: &str = r#"
id: kitchen-fan-hot
description: Turn kitchen fan on when temp > 25 C.
triggers:
  - device.zigbee2mqtt.kitchen-temp.temperature.state
when: "payload.value > 25"
actions:
  - !publish
    subject: cmd.zigbee2mqtt.kitchen-fan.on
    iot_type: iot.device.v1.Command
"#;

    fn write_rule(dir: &Path, body: &str) -> PathBuf {
        let src = dir.join("r.yaml");
        fs::write(&src, body).unwrap();
        src
    }

    #[test]
    fn add_rejects_collision_without_force() {
        let src_dir = tempfile::tempdir().unwrap();
        let rules_dir = tempfile::tempdir().unwrap();
        let src = write_rule(src_dir.path(), DEMO_RULE);

        add(&src, rules_dir.path(), false).unwrap();
        let err = add(&src, rules_dir.path(), false).unwrap_err();
        assert!(format!("{err:#}").contains("already installed"));
        // With force it succeeds.
        add(&src, rules_dir.path(), true).unwrap();
    }

    #[test]
    fn list_picks_up_installed_rule() {
        let src_dir = tempfile::tempdir().unwrap();
        let rules_dir = tempfile::tempdir().unwrap();
        let src = write_rule(src_dir.path(), DEMO_RULE);
        add(&src, rules_dir.path(), false).unwrap();

        // list() prints to stdout — just confirm it returns Ok and
        // the file landed where expected.
        list(rules_dir.path()).unwrap();
        assert!(rules_dir.path().join("kitchen-fan-hot.yaml").is_file());
    }

    #[test]
    fn delete_removes_rule() {
        let src_dir = tempfile::tempdir().unwrap();
        let rules_dir = tempfile::tempdir().unwrap();
        let src = write_rule(src_dir.path(), DEMO_RULE);
        add(&src, rules_dir.path(), false).unwrap();
        delete("kitchen-fan-hot", rules_dir.path()).unwrap();
        assert!(!rules_dir.path().join("kitchen-fan-hot.yaml").exists());

        // Second delete should fail with a clear message.
        let err = delete("kitchen-fan-hot", rules_dir.path()).unwrap_err();
        assert!(format!("{err:#}").contains("not installed"));
    }

    #[test]
    fn read_payload_accepts_inline_and_file() {
        let inline = read_payload(r#"{"value":25}"#).unwrap();
        assert_eq!(inline["value"], 25);

        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(tmp.path(), r#"{"value":99}"#).unwrap();
        let from_file = read_payload(&format!("@{}", tmp.path().display())).unwrap();
        assert_eq!(from_file["value"], 99);
    }
}
