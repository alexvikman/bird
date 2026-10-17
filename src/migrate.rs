//! Migrations for Bird's own files in `~/.bird` (config.toml, installed PHP
//! versions, and anything stored later), applied when Bird is updated.
//!
//! Like database migrations:
//! - Each migration has a sortable id (`YYYY_MM_DD_NNNNNN_name`) and runs once;
//!   applied ids are recorded in `~/.bird/migrations.json`.
//! - Forward only, and idempotent: a migration must leave things correct even
//!   if it runs again (e.g. after a crash before it was recorded). There is no
//!   "down"; `config.toml` is backed up to `~/.bird/backups/` first.
//! - Never change a released migration; add a new one.
//! - They run automatically: when the daemon starts, before CLI commands
//!   (never as root, never in the php/composer shims) and right after
//!   `bird self-update`, under a lock so two processes never run them at once.
//! - A Bird older than the data (it sees ids it doesn't know) warns instead of
//!   pretending to be current.

use std::collections::HashSet;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::paths::paths;
use crate::ui;

pub struct Migration {
    pub id: &'static str,
    pub description: &'static str,
    /// Gets Bird's home directory (`~/.bird`).
    pub run: fn(&Path) -> Result<()>,
}

/// All migrations, oldest first. Append only.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        id: "2026_10_17_000001_php_mirror_default",
        description: "Use Bird's PHP builds instead of the dl.static-php.dev default from Bird 0.1",
        run: php_mirror_default,
    },
    Migration {
        id: "2026_10_17_000002_record_php_sources",
        description: "Record that PHP versions installed before Bird 0.2 are static-php builds",
        run: record_php_sources,
    },
];

// ---------------------------------------------------------------------------
// The migrations

const OLD_DEFAULT_MIRROR: &str = "https://dl.static-php.dev/static-php-cli/common";

/// Bird 0.1 wrote its default mirror into every config.toml. Moves those to
/// Bird's own builds; a static-php URL someone chose stays as it is.
fn php_mirror_default(home: &Path) -> Result<()> {
    let path = home.join("config.toml");
    if !path.exists() {
        return Ok(());
    }
    let mut cfg = Config::load_from(&path)?;
    if cfg.php_mirror == OLD_DEFAULT_MIRROR {
        cfg.php_mirror = crate::php::DEFAULT_MIRROR.into();
        cfg.save_to(&path)?;
    }
    Ok(())
}

/// PHP installed before Bird had its own builds has no SOURCE file; write it,
/// so nothing has to guess.
fn record_php_sources(home: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(home.join("php")) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if dir.join("bin/php").is_file() && !dir.join("SOURCE").exists() {
            std::fs::write(
                dir.join("SOURCE"),
                format!("{}\n", crate::php::SOURCE_STATIC_PHP),
            )?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bookkeeping

#[derive(Default, Serialize, Deserialize)]
struct State {
    applied: Vec<Applied>,
}

#[derive(Serialize, Deserialize)]
struct Applied {
    id: String,
    /// Unix time.
    applied_at: u64,
    /// The Bird version that applied it.
    bird_version: String,
}

fn state_path(home: &Path) -> PathBuf {
    home.join("migrations.json")
}

fn load_state(home: &Path) -> Result<State> {
    match std::fs::read_to_string(state_path(home)) {
        Ok(text) => serde_json::from_str(&text).context("could not parse migrations.json"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(e).context("could not read migrations.json"),
    }
}

fn save_state(home: &Path, state: &State) -> Result<()> {
    let path = state_path(home);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state)? + "\n")?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Exclusive lock for the duration of a run: held by keeping the file open,
/// released when it is dropped (closed).
struct Lock(#[allow(dead_code)] std::fs::File);

fn lock(home: &Path) -> Result<Lock> {
    let path = home.join("migrations.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("could not open {}", path.display()))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error()).context("could not lock migrations");
    }
    Ok(Lock(file))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Copies config.toml to backups/ before migrating; keeps the last 10.
fn backup_config(home: &Path, before: &str) -> Result<()> {
    let config = home.join("config.toml");
    if !config.exists() {
        return Ok(());
    }
    let dir = home.join("backups");
    std::fs::create_dir_all(&dir)?;
    std::fs::copy(
        &config,
        dir.join(format!("config-{}-before-{before}.toml", now())),
    )?;
    let mut backups: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("config-"))
        })
        .collect();
    backups.sort();
    let excess = backups.len().saturating_sub(10);
    for old in &backups[..excess] {
        let _ = std::fs::remove_file(old);
    }
    Ok(())
}

pub struct Outcome {
    pub applied: Vec<&'static str>,
    /// Ids in migrations.json this Bird doesn't know: the data is newer.
    pub unknown: Vec<String>,
}

/// Runs the pending migrations in `home`.
pub fn run_in(home: &Path, migrations: &[Migration]) -> Result<Outcome> {
    if !home.is_dir() {
        // Not installed yet; `bird install` runs them once it created ~/.bird.
        return Ok(Outcome {
            applied: Vec::new(),
            unknown: Vec::new(),
        });
    }
    let _lock = lock(home)?;
    let mut state = load_state(home)?;
    let known: HashSet<&str> = migrations.iter().map(|m| m.id).collect();
    let applied: HashSet<String> = state.applied.iter().map(|a| a.id.clone()).collect();
    let unknown: Vec<String> = applied
        .iter()
        .filter(|id| !known.contains(id.as_str()))
        .cloned()
        .collect();
    let pending: Vec<&Migration> = migrations
        .iter()
        .filter(|m| !applied.contains(m.id))
        .collect();
    let mut done = Vec::new();
    if let Some(first) = pending.first() {
        backup_config(home, first.id)?;
    }
    for m in pending {
        (m.run)(home).with_context(|| {
            format!(
                "migration {} failed; config.toml backups are in {}",
                m.id,
                home.join("backups").display()
            )
        })?;
        state.applied.push(Applied {
            id: m.id.to_string(),
            applied_at: now(),
            bird_version: env!("CARGO_PKG_VERSION").to_string(),
        });
        // Record each one right away, so a later failure keeps earlier progress.
        save_state(home, &state)?;
        done.push(m.id);
    }
    Ok(Outcome {
        applied: done,
        unknown,
    })
}

/// Runs pending migrations in `~/.bird`. Never as root: files in ~/.bird must
/// stay the user's.
pub fn run() -> Result<Outcome> {
    if unsafe { libc::geteuid() } == 0 {
        return Ok(Outcome {
            applied: Vec::new(),
            unknown: Vec::new(),
        });
    }
    run_in(&paths().home, MIGRATIONS)
}

/// Runs pending migrations before a CLI command, telling the user briefly.
pub fn run_for_cli() -> Result<()> {
    let outcome = run()?;
    for id in &outcome.applied {
        let description = MIGRATIONS
            .iter()
            .find(|m| m.id == *id)
            .map_or("", |m| m.description);
        eprintln!("{} {}", ui::dim("migrated:"), ui::dim(description));
    }
    if !outcome.unknown.is_empty() {
        eprintln!(
            "{} ~/.bird was migrated by a newer Bird ({}); run `bird self-update`",
            ui::yellow("warning:"),
            outcome.unknown.join(", ")
        );
    }
    Ok(())
}

/// `bird migrate --status`
pub fn print_status() -> Result<()> {
    let state = load_state(&paths().home)?;
    let rows: Vec<Vec<String>> = MIGRATIONS
        .iter()
        .map(|m| {
            let status = match state.applied.iter().find(|a| a.id == m.id) {
                Some(a) => ui::green(&format!("applied by {}", a.bird_version)),
                None => ui::yellow("pending"),
            };
            vec![m.id.to_string(), status, m.description.to_string()]
        })
        .collect();
    ui::table(&["MIGRATION", "STATUS", "WHAT"], &rows);
    let known: HashSet<&str> = MIGRATIONS.iter().map(|m| m.id).collect();
    for a in state
        .applied
        .iter()
        .filter(|a| !known.contains(a.id.as_str()))
    {
        ui::warn(format!(
            "{} was applied by Bird {} (newer than this one)",
            a.id, a.bird_version
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bird-migrate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn ids_are_unique_and_sorted() {
        let ids: Vec<&str> = MIGRATIONS.iter().map(|m| m.id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(ids, sorted, "migration ids must be unique and in order");
        for id in ids {
            assert!(
                id.len() > 18 && id.as_bytes()[4] == b'_',
                "id {id} should start with YYYY_MM_DD_NNNNNN_"
            );
        }
    }

    #[test]
    fn runs_once_records_and_backs_up() {
        let home = temp_home("once");
        std::fs::write(
            home.join("config.toml"),
            format!("php_mirror = \"{OLD_DEFAULT_MIRROR}\"\n"),
        )
        .unwrap();
        std::fs::create_dir_all(home.join("php/8.3/bin")).unwrap();
        std::fs::write(home.join("php/8.3/bin/php"), "").unwrap();

        let first = run_in(&home, MIGRATIONS).unwrap();
        assert_eq!(first.applied.len(), MIGRATIONS.len());
        let cfg = Config::load_from(&home.join("config.toml")).unwrap();
        assert_eq!(cfg.php_mirror, crate::php::DEFAULT_MIRROR);
        assert_eq!(
            std::fs::read_to_string(home.join("php/8.3/SOURCE"))
                .unwrap()
                .trim(),
            "static-php"
        );
        assert_eq!(std::fs::read_dir(home.join("backups")).unwrap().count(), 1);

        let second = run_in(&home, MIGRATIONS).unwrap();
        assert!(second.applied.is_empty(), "nothing runs twice");
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn keeps_a_chosen_mirror_and_skips_missing_files() {
        let home = temp_home("keep");
        let custom = "https://dl.static-php.dev/static-php-cli/bulk";
        std::fs::write(
            home.join("config.toml"),
            format!("php_mirror = \"{custom}\"\n"),
        )
        .unwrap();
        run_in(&home, MIGRATIONS).unwrap();
        assert_eq!(
            Config::load_from(&home.join("config.toml"))
                .unwrap()
                .php_mirror,
            custom
        );

        let empty = temp_home("empty");
        run_in(&empty, MIGRATIONS).unwrap();
        assert!(
            !empty.join("config.toml").exists(),
            "no config is created out of nothing"
        );
        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(empty).ok();
    }

    fn failing(_: &Path) -> Result<()> {
        anyhow::bail!("boom")
    }
    fn ok(_: &Path) -> Result<()> {
        Ok(())
    }

    #[test]
    fn a_failure_keeps_earlier_progress() {
        let home = temp_home("fail");
        let list = [
            Migration {
                id: "2026_01_01_000001_a",
                description: "a",
                run: ok,
            },
            Migration {
                id: "2026_01_01_000002_b",
                description: "b",
                run: failing,
            },
            Migration {
                id: "2026_01_01_000003_c",
                description: "c",
                run: ok,
            },
        ];
        assert!(run_in(&home, &list).is_err());
        let state = load_state(&home).unwrap();
        let ids: Vec<&str> = state.applied.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["2026_01_01_000001_a"]);
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn notices_newer_data() {
        let home = temp_home("newer");
        let newer = [
            Migration {
                id: "2026_01_01_000001_a",
                description: "a",
                run: ok,
            },
            Migration {
                id: "2030_01_01_000001_future",
                description: "f",
                run: ok,
            },
        ];
        run_in(&home, &newer).unwrap();
        let older = [Migration {
            id: "2026_01_01_000001_a",
            description: "a",
            run: ok,
        }];
        let outcome = run_in(&home, &older).unwrap();
        assert_eq!(outcome.unknown, ["2030_01_01_000001_future"]);
        std::fs::remove_dir_all(home).ok();
    }
}
