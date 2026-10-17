//! Keeps ~/.bird/log from growing without end. The daemon runs `tidy` every
//! few minutes:
//!
//! - A log over `LOG_LIMIT` is copied to `<name>.1` (replacing the previous
//!   one) and emptied in place. launchd (bird.log) and php-fpm append to these
//!   files, so they keep writing to the same file; renaming it away would
//!   leave them writing to the old one.
//! - Xdebug profiles and traces (log/xdebug) older than `XDEBUG_MAX_AGE` are
//!   deleted, and the oldest go first while the folder is over `XDEBUG_LIMIT`.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const LOG_LIMIT: u64 = 10 * 1024 * 1024;
pub const XDEBUG_LIMIT: u64 = 1024 * 1024 * 1024;
pub const XDEBUG_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub fn tidy(dir: &Path) -> std::io::Result<()> {
    rotate_logs(dir, LOG_LIMIT)?;
    prune(&dir.join("xdebug"), XDEBUG_MAX_AGE, XDEBUG_LIMIT)
}

fn rotate_logs(dir: &Path, limit: u64) -> std::io::Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || path.extension().is_none_or(|e| e != "log") || meta.len() <= limit {
            continue;
        }
        let mut old = path.clone().into_os_string();
        old.push(".1");
        fs::copy(&path, PathBuf::from(old))?;
        OpenOptions::new().write(true).open(&path)?.set_len(0)?;
    }
    Ok(())
}

fn prune(dir: &Path, max_age: Duration, limit: u64) -> std::io::Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    let now = SystemTime::now();
    let mut kept = Vec::new();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(now);
        if now.duration_since(modified).unwrap_or_default() > max_age {
            fs::remove_file(entry.path())?;
        } else {
            kept.push((modified, meta.len(), entry.path()));
        }
    }
    let mut total: u64 = kept.iter().map(|(_, len, _)| len).sum();
    kept.sort();
    for (_, len, path) in kept {
        if total <= limit {
            break;
        }
        fs::remove_file(path)?;
        total -= len;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bird-logs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_big_log_is_kept_once_and_emptied_in_place() {
        let dir = scratch("rotate");
        let log = dir.join("bird.log");
        fs::write(&log, "x".repeat(200)).unwrap();
        fs::write(dir.join("small.log"), "ok").unwrap();
        fs::write(dir.join("notes.txt"), "y".repeat(200)).unwrap();
        // A writer that appends, like launchd and php-fpm.
        let mut writer = OpenOptions::new().append(true).open(&log).unwrap();

        rotate_logs(&dir, 100).unwrap();
        std::io::Write::write_all(&mut writer, b"next\n").unwrap();

        assert_eq!(fs::read_to_string(&log).unwrap(), "next\n");
        assert_eq!(
            fs::read_to_string(dir.join("bird.log.1")).unwrap().len(),
            200
        );
        assert_eq!(fs::read_to_string(dir.join("small.log")).unwrap(), "ok");
        assert_eq!(
            fs::read_to_string(dir.join("notes.txt")).unwrap().len(),
            200
        );
        assert!(!dir.join("small.log.1").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn xdebug_output_is_pruned_by_age_then_size() {
        let dir = scratch("prune");
        let now = SystemTime::now();
        let file = |name: &str, len: usize, age_secs: u64| {
            let path = dir.join(name);
            fs::write(&path, vec![0u8; len]).unwrap();
            let f = OpenOptions::new().write(true).open(&path).unwrap();
            f.set_modified(now - Duration::from_secs(age_secs)).unwrap();
        };
        file("cachegrind.out.old", 10, 3600 * 24 * 30);
        file("cachegrind.out.1", 40, 300);
        file("cachegrind.out.2", 40, 200);
        file("cachegrind.out.3", 40, 100);

        prune(&dir, Duration::from_secs(3600 * 24 * 7), 100).unwrap();

        assert!(
            !dir.join("cachegrind.out.old").exists(),
            "older than the max age"
        );
        assert!(
            !dir.join("cachegrind.out.1").exists(),
            "oldest, over the size limit"
        );
        assert!(dir.join("cachegrind.out.2").exists());
        assert!(dir.join("cachegrind.out.3").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
