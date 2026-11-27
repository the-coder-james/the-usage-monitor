//! Local usage history: an append-only log of readings for the popover's sparklines.
//!
//! Built to cost next to nothing. A reading is written only when a percentage
//! changes, or as a heartbeat every 30 minutes. Percentages are whole numbers,
//! so a busy day writes a couple of hundred short lines and an idle one writes
//! 48. Nothing is ever sent anywhere; the file never leaves the machine.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Write a reading even when it has not changed, once this long has passed.
///
/// Without it, "usage held steady for three hours" and "the app was not
/// running for three hours" leave identical files, and the chart would draw a
/// confident flat line across a gap. With it, any silence longer than this
/// (plus a poll's slack) means no reading was taken.
pub const HEARTBEAT_SECS: i64 = 30 * 60;

/// How long readings are kept. The weekly chart spans 7 days; the extra day
/// guarantees a reading from before its left edge, so the line starts at the
/// edge rather than partway in.
pub const RETENTION_SECS: i64 = 8 * 24 * 3600;

/// Expired lines are dropped from memory immediately but from the file only
/// once this many have built up, so the file is rewritten every few days
/// rather than on every poll.
const COMPACT_AFTER: usize = 512;

// MARK: - Model

/// One reading, as written to the file: `epoch_secs,session,weekly`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub t: i64,
    pub session: u8,
    pub weekly: u8,
}

impl Sample {
    /// `None` for anything malformed, so a truncated last line (a crash
    /// mid-write) costs one reading rather than the whole history.
    fn parse(line: &str) -> Option<Self> {
        let mut parts = line.trim().split(',');
        let sample = Self {
            t: parts.next()?.parse().ok()?,
            session: parts.next()?.parse().ok()?,
            weekly: parts.next()?.parse().ok()?,
        };
        parts.next().is_none().then_some(sample)
    }

    fn line(&self) -> String {
        format!("{},{},{}\n", self.t, self.session, self.weekly)
    }
}

/// What the caller must do to the file after a `record`.
#[derive(Debug, PartialEq, Eq)]
pub enum Persist {
    Nothing,
    Append(Sample),
    /// Enough lines have expired that the file should be rewritten from memory.
    Rewrite,
}

/// The in-memory history. Pure: every method takes `now`, and file I/O is
/// left to the caller, so tests never touch the real file.
#[derive(Debug, Default)]
pub struct History {
    /// Oldest first.
    samples: Vec<Sample>,
    /// Lines still in the file that have expired from memory.
    expired_in_file: usize,
}

impl History {
    /// Loads the file's contents, returning whether the file should be
    /// rewritten because it held expired or malformed lines.
    pub fn from_file_contents(text: &str, now: i64) -> (Self, bool) {
        let mut samples: Vec<Sample> = text.lines().filter_map(Sample::parse).collect();
        // A wall-clock change can append a reading out of order; `since`
        // relies on sorted samples.
        samples.sort_by_key(|s| s.t);
        let mut history = Self { samples, expired_in_file: 0 };
        history.expire(now);
        let dirty = history.samples.len() != text.lines().count();
        history.expired_in_file = 0;
        (history, dirty)
    }

    /// Records a reading if it is worth keeping.
    pub fn record(&mut self, now: i64, session: u8, weekly: u8) -> Persist {
        let due = self.samples.last().is_none_or(|last| {
            last.session != session || last.weekly != weekly || now - last.t >= HEARTBEAT_SECS
        });
        if !due {
            return Persist::Nothing;
        }

        let sample = Sample { t: now, session, weekly };
        self.samples.push(sample);
        self.expired_in_file += self.expire(now);
        if self.expired_in_file >= COMPACT_AFTER {
            self.expired_in_file = 0;
            Persist::Rewrite
        } else {
            Persist::Append(sample)
        }
    }

    /// Readings at or after `from`, plus the last one before it: the chart's
    /// line has to start at its left edge with whatever value held there.
    pub fn since(&self, from: i64) -> &[Sample] {
        let start = self.samples.partition_point(|s| s.t < from);
        &self.samples[start.saturating_sub(1)..]
    }

    /// The whole history in file format.
    pub fn file_contents(&self) -> String {
        self.samples.iter().map(Sample::line).collect()
    }

    /// Drops readings past retention, returning how many went.
    fn expire(&mut self, now: i64) -> usize {
        let keep_from = self.samples.partition_point(|s| s.t < now - RETENTION_SECS);
        self.samples.drain(..keep_from).count()
    }
}

// MARK: - File

/// Loaded on first use, then kept in memory: about 16 bytes a reading, so a
/// full week is a few tens of kilobytes at most.
static HISTORY: Mutex<Option<History>> = Mutex::new(None);

fn history_path() -> Option<PathBuf> {
    Some(crate::settings::data_dir()?.join("history.csv"))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Runs `f` against the history, loading it from disk on first use. The lock
/// is held across the file write too, so a forced refresh racing the poller
/// cannot interleave an append with a rewrite.
fn with_history<T>(f: impl FnOnce(&mut History, Option<&Path>) -> T) -> T {
    let path = history_path();
    let mut guard = HISTORY.lock().expect("history lock");
    let history = guard.get_or_insert_with(|| {
        let text = path.as_deref().and_then(|p| std::fs::read_to_string(p).ok());
        let (history, dirty) =
            History::from_file_contents(text.as_deref().unwrap_or(""), now_secs());
        if let (true, Some(p)) = (dirty, path.as_deref()) {
            rewrite(p, &history);
        }
        history
    });
    f(history, path.as_deref())
}

/// Records one successful poll. A write failure is logged and otherwise
/// ignored: history is a nicety and must never affect the usage reading.
pub fn record(session_percent: f64, weekly_percent: f64) {
    // Percentages arrive already rounded; this only guards the cast.
    let pct = |p: f64| p.round().clamp(0.0, u8::MAX as f64) as u8;
    with_history(|history, path| {
        let Some(path) = path else { return };
        match history.record(now_secs(), pct(session_percent), pct(weekly_percent)) {
            Persist::Nothing => {}
            Persist::Append(sample) => append(path, &sample),
            Persist::Rewrite => rewrite(path, history),
        }
    });
}

/// Readings since `from_ms`, as `[epoch_ms, session, weekly]` triples. A
/// triple, not an object, keeps the IPC payload about a third the size.
pub fn since_ms(from_ms: i64) -> Vec<[i64; 3]> {
    with_history(|history, _| {
        history
            .since(from_ms / 1000)
            .iter()
            .map(|s| [s.t * 1000, s.session as i64, s.weekly as i64])
            .collect()
    })
}

fn append(path: &Path, sample: &Sample) {
    let write = std::fs::create_dir_all(path.parent().unwrap_or(path)).and_then(|_| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(sample.line().as_bytes())
    });
    if let Err(e) = write {
        eprintln!("could not append usage history: {}", e.kind());
    }
}

/// Write-then-rename, so a crash mid-rewrite leaves the old file intact rather
/// than a truncated one.
fn rewrite(path: &Path, history: &History) {
    let tmp = path.with_extension("csv.tmp");
    let write = std::fs::create_dir_all(path.parent().unwrap_or(path))
        .and_then(|_| std::fs::write(&tmp, history.file_contents()))
        .and_then(|_| std::fs::rename(&tmp, path));
    if let Err(e) = write {
        eprintln!("could not rewrite usage history: {}", e.kind());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000;

    #[test]
    fn unchanged_readings_are_not_written_until_the_heartbeat() {
        let mut h = History::default();
        assert!(matches!(h.record(T0, 10, 20), Persist::Append(_)));
        assert_eq!(h.record(T0 + 60, 10, 20), Persist::Nothing);
        assert_eq!(h.record(T0 + HEARTBEAT_SECS - 1, 10, 20), Persist::Nothing);
        assert!(matches!(h.record(T0 + HEARTBEAT_SECS, 10, 20), Persist::Append(_)));
    }

    #[test]
    fn a_change_in_either_window_is_written_at_once() {
        let mut h = History::default();
        h.record(T0, 10, 20);
        assert!(matches!(h.record(T0 + 60, 11, 20), Persist::Append(_)));
        assert!(matches!(h.record(T0 + 120, 11, 21), Persist::Append(_)));
    }

    #[test]
    fn since_includes_the_reading_that_held_at_the_left_edge() {
        let mut h = History::default();
        h.record(T0, 10, 20);
        h.record(T0 + 100, 30, 20);
        h.record(T0 + 200, 50, 20);
        let got: Vec<i64> = h.since(T0 + 150).iter().map(|s| s.t).collect();
        assert_eq!(got, [T0 + 100, T0 + 200]);
        // Before all readings: everything, with nothing to anchor.
        assert_eq!(h.since(0).len(), 3);
    }

    #[test]
    fn expired_lines_trigger_a_rewrite_only_in_bulk() {
        let mut h = History::default();
        // Heartbeat-spaced readings, enough to span past retention.
        let mut t = T0;
        let mut rewrites = 0;
        for _ in 0..(RETENTION_SECS / HEARTBEAT_SECS + COMPACT_AFTER as i64 + 1) {
            if h.record(t, 1, 1) == Persist::Rewrite {
                rewrites += 1;
            }
            t += HEARTBEAT_SECS;
        }
        assert_eq!(rewrites, 1, "one compaction per COMPACT_AFTER expiries, not one per poll");
        assert!(h.samples.first().unwrap().t >= t - HEARTBEAT_SECS - RETENTION_SECS);
    }

    #[test]
    fn loading_drops_expired_and_malformed_lines() {
        let text = format!(
            "{},5,5\n{},10,20\ngarbage\n{},11,20\n{},12",
            T0 - RETENTION_SECS - 1,
            T0 - 100,
            T0 - 50,
            T0 - 10,
        );
        let (h, dirty) = History::from_file_contents(&text, T0);
        assert!(dirty);
        assert_eq!(h.file_contents(), format!("{},10,20\n{},11,20\n", T0 - 100, T0 - 50));
    }

    #[test]
    fn a_clean_file_is_not_rewritten_on_load() {
        let text = format!("{},10,20\n{},11,20\n", T0 - 100, T0 - 50);
        let (h, dirty) = History::from_file_contents(&text, T0);
        assert!(!dirty);
        assert_eq!(h.file_contents(), text);
    }

    #[test]
    fn out_of_order_lines_are_sorted_on_load() {
        let text = format!("{},11,20\n{},10,20\n", T0 - 50, T0 - 100);
        let (h, _) = History::from_file_contents(&text, T0);
        assert_eq!(h.since(0)[0].t, T0 - 100);
    }
}
