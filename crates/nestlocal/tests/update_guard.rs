//! rootfs/nestlocal-update-guard against a unit's tree in a temp dir.
#![cfg(unix)]

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

const GUARD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/rootfs/nestlocal-update-guard");
const JOURNAL: &str = "etc/nestlocal/update/journal";
const STAGING: &str = "etc/nestlocal/update/staging";

/// The journal of the set `Unit::swapped` leaves: nestlocal and its options
/// replaced, `etc/added/file` created in a directory of its own, `etc/held`
/// made 700 from 755, and `etc/monitrc` removed.
fn journal(version: u32, class: &str, state: &str) -> String {
    let meta = std::fs::metadata(std::env::current_exe().unwrap()).unwrap();
    let owner = format!("{}:{}", meta.uid(), meta.gid());
    format!("unfr0zen-update {version}\nclass {class}\nstate {state}\nmkdir etc/added\nowner etc/held {owner} 755\n\
             write nestlabs/sbin/nestlocal nestlabs/sbin/nestlocal.unfr0zen-previous\n\
             write etc/nestlocal/options etc/nestlocal/options.unfr0zen-previous\n\
             write etc/added/file -\nremove etc/monitrc etc/monitrc.unfr0zen-previous\n")
}

/// A unit's tree, with or without a set applied over Wi-Fi.
struct Unit {
    root: PathBuf,
    /// What the unit's logger does: logs to `log`, or fails.
    logger: &'static str,
    /// The guard stops, as at a power cut, after this many of its moves.
    moves: Option<u32>,
}

impl Unit {
    /// A unit with nothing applied: nestlocal, its options and monitrc "old".
    fn new(name: &str) -> Unit {
        let root = std::env::temp_dir().join(format!("nestlocal-guard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["nestlabs/sbin", "media/data", "var/run", "bin", "etc/nestlocal/update"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let unit = Unit { root, logger: "log", moves: None };
        for file in ["nestlabs/sbin/nestlocal", "etc/nestlocal/options", "etc/monitrc"] {
            unit.put(file, "old");
        }
        std::fs::create_dir(unit.path("etc/held")).unwrap();
        unit.held(0o755);
        unit
    }

    /// A unit with journal's set swapped in, of class, on trial if pending.
    fn swapped(name: &str, class: &str, pending: bool) -> Unit {
        let unit = Unit::new(name);
        for file in ["nestlabs/sbin/nestlocal", "etc/nestlocal/options"] {
            std::fs::rename(unit.path(file), unit.path(&format!("{file}.unfr0zen-previous"))).unwrap();
            unit.put(file, "new");
        }
        std::fs::rename(unit.path("etc/monitrc"), unit.path("etc/monitrc.unfr0zen-previous")).unwrap();
        std::fs::create_dir(unit.path("etc/added")).unwrap();
        unit.put("etc/added/file", "new");
        unit.held(0o700);
        unit.put(JOURNAL, &journal(1, class, "swapped"));
        if pending {
            unit.put("media/data/nestlocal-update.pending", "0\n");
        }
        unit
    }

    /// A unit whose logger fails, as one without a logger would.
    fn without_logger(mut self) -> Unit {
        self.logger = "fail";
        self
    }

    /// A unit whose guard stops after its first moves.
    fn cut_after(mut self, moves: u32) -> Unit {
        self.moves = Some(moves);
        self
    }

    /// Sets `etc/held`'s mode.
    fn held(&self, mode: u32) {
        std::fs::set_permissions(self.path("etc/held"), std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn held_mode(&self) -> u32 {
        std::fs::metadata(self.path("etc/held")).unwrap().mode() & 0o7777
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn put(&self, name: &str, content: &str) {
        std::fs::write(self.path(name), content).unwrap();
    }

    fn get(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.path(name)).ok()
    }

    /// `sh` running the script `args` names with the rest of them, in this
    /// unit's environment for the guard. logger and mv are functions, since
    /// a busybox sh runs its own applet before any on PATH.
    fn sh(&self, wait: u32, args: &[&str]) -> Command {
        let logger = match self.logger {
            "fail" => "return 127".to_string(),
            _ => format!("echo \"$*\" >>'{}'", self.root.join("log").display()),
        };
        let mv = match self.moves {
            Some(moves) => format!("mv() {{ command mv \"$@\" || return; MOVES=$((${{MOVES:-0}} + 1)); \
                                    [ \"$MOVES\" -lt {moves} ] || exit 9; }}; "),
            None => String::new(),
        };
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(format!("logger() {{ {logger}; }}; {mv}. \"$0\""))
            .args(args)
            .env("NESTLOCAL_UPDATE_ROOT", &self.root)
            .env("NESTLOCAL_UPDATE_WAIT", wait.to_string())
            .env("NESTLOCAL_UPDATE_RESTART", format!("echo restart >>{}", self.restarted().display()))
            .env("NESTLOCAL_UPDATE_REBOOT", format!("echo reboot >>{}", self.rebooted().display()));
        command
    }

    fn guard(&self, wait: u32) {
        let out = self.sh(wait, &[GUARD, "start"]).output().unwrap();
        assert!(out.status.success(), "guard failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// What nestlocal's keep does: the marker, then the backups, then the
    /// journal.
    fn keep(&self) {
        std::fs::remove_file(self.marker()).unwrap();
        for backup in ["nestlabs/sbin/nestlocal", "etc/nestlocal/options", "etc/monitrc"] {
            std::fs::remove_file(self.path(&format!("{backup}.unfr0zen-previous"))).unwrap();
        }
        std::fs::remove_file(self.path(JOURNAL)).unwrap();
    }

    fn binary(&self) -> String {
        self.get("nestlabs/sbin/nestlocal").unwrap()
    }

    fn marker(&self) -> PathBuf {
        self.root.join("media/data/nestlocal-update.pending")
    }

    /// What the guard records for nestlocal once it put a set back.
    fn rolled_back(&self) -> PathBuf {
        self.root.join("media/data/nestlocal-update.rolled-back")
    }

    fn restarted(&self) -> PathBuf {
        self.root.join("restarted")
    }

    fn rebooted(&self) -> PathBuf {
        self.root.join("rebooted")
    }

    fn restarts(&self) -> usize {
        std::fs::read_to_string(self.restarted()).map_or(0, |s| s.lines().count())
    }

    fn watcher(&self) -> Option<String> {
        self.get("var/run/nestlocal-update-guard.pid")
    }

    fn logged(&self) -> String {
        self.get("log").unwrap_or_default()
    }

    /// Every file is as before the set, nothing it created or kept is left,
    /// and the set is forgotten.
    fn assert_rolled_back(&self) {
        assert_eq!(self.binary(), "old");
        assert_eq!(self.get("etc/nestlocal/options").as_deref(), Some("old"));
        assert_eq!(self.get("etc/monitrc").as_deref(), Some("old"));
        assert!(!self.path("etc/added").exists(), "a directory the set created is left");
        assert_eq!(self.held_mode(), 0o755, "a directory the set changed keeps the set's mode");
        for backup in ["nestlabs/sbin/nestlocal", "etc/nestlocal/options", "etc/monitrc"] {
            assert!(!self.path(&format!("{backup}.unfr0zen-previous")).exists(), "{backup}'s backup is left");
        }
        assert!(!self.path(JOURNAL).exists() && !self.marker().exists());
        assert!(self.rolled_back().exists(), "nestlocal is not told the set was put back");
    }

    /// The set is in place and nothing of its rollback is left.
    fn assert_kept(&self) {
        assert_eq!(self.binary(), "new");
        assert_eq!(self.get("etc/nestlocal/options").as_deref(), Some("new"));
        assert_eq!(self.get("etc/added/file").as_deref(), Some("new"));
        assert!(!self.path("etc/monitrc").exists());
        assert_eq!(self.held_mode(), 0o700);
        for backup in ["nestlabs/sbin/nestlocal", "etc/nestlocal/options", "etc/monitrc"] {
            assert!(!self.path(&format!("{backup}.unfr0zen-previous")).exists(), "{backup}'s backup is left");
        }
        assert!(!self.path(JOURNAL).exists() && !self.marker().exists());
        assert!(!self.rolled_back().exists());
    }
}

impl Drop for Unit {
    // Without the marker, a watcher still running ends within a second.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.marker());
        std::thread::sleep(Duration::from_millis(1500));
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// How long a test waits for what a 1-3 s watch does: ample on a loaded
/// machine, and a passing test waits only as long as the watch takes.
const WATCHED: Duration = Duration::from_secs(15);

fn until(limit: Duration, done: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    done()
}

#[test]
fn no_set_changes_nothing() {
    let unit = Unit::new("none");
    unit.guard(600);
    assert_eq!(unit.binary(), "old");
    assert!(!unit.marker().exists());
    assert!(unit.watcher().is_none());
    assert_eq!(unit.logged(), "");
}

#[test]
fn rolls_back_after_three_starts() {
    let unit = Unit::swapped("starts", "restart", true);
    unit.guard(600);
    let watcher = unit.watcher();
    assert!(watcher.is_some());
    for count in 2..=3 {
        unit.guard(600);
        assert_eq!(unit.binary(), "new");
        assert_eq!(std::fs::read_to_string(unit.marker()).unwrap(), format!("{count}\n"));
        assert_eq!(unit.watcher(), watcher, "one watcher per update");
    }
    unit.guard(600);
    unit.assert_rolled_back();
    assert_eq!(unit.logged(), "-t nestlocal update rolled back\n");
    assert!(!unit.restarted().exists(), "the start under way runs the old nestlocal");
    assert!(!unit.rebooted().exists());
}

#[test]
fn a_failed_trial_restores_every_file_and_removes_created_ones() {
    let unit = Unit::swapped("watch", "restart", true);
    unit.guard(1);
    assert!(until(WATCHED, || unit.restarted().exists()), "the watch never restarted nestlocal");
    unit.assert_rolled_back();
    assert_eq!(unit.logged(), "-t nestlocal update rolled back\n");
    assert!(!unit.rebooted().exists());
}

#[test]
fn a_reboot_class_rollback_reboots() {
    let unit = Unit::swapped("reboot", "reboot", true);
    unit.guard(1);
    assert!(until(WATCHED, || unit.rebooted().exists()), "the watch never rebooted the unit");
    unit.assert_rolled_back();
    assert!(!unit.restarted().exists());
    // Past three starts, too.
    let unit = Unit::swapped("rebootstarts", "reboot", true);
    std::fs::write(unit.marker(), "3\n").unwrap();
    unit.guard(600);
    unit.assert_rolled_back();
    assert_eq!(std::fs::read_to_string(unit.rebooted()).unwrap(), "reboot\n");
}

#[test]
fn a_journal_left_swapping_restores_every_file() {
    // A power cut part way through the swap: every backup linked, nestlocal
    // renamed in, options not yet (its backup is a second link to it),
    // etc/added made with its file still staged, etc/held changed, monitrc
    // not yet removed.
    let unit = Unit::new("swapping");
    unit.held(0o700);
    std::fs::rename(unit.path("nestlabs/sbin/nestlocal"), unit.path("nestlabs/sbin/nestlocal.unfr0zen-previous")).unwrap();
    unit.put("nestlabs/sbin/nestlocal", "new");
    std::fs::hard_link(unit.path("etc/nestlocal/options"), unit.path("etc/nestlocal/options.unfr0zen-previous")).unwrap();
    std::fs::create_dir(unit.path("etc/added")).unwrap();
    unit.put("etc/added/file.unfr0zen-new", "new");
    unit.put(STAGING, "mkdir etc/added\nnew nestlabs/sbin/nestlocal.unfr0zen-new\nnew etc/nestlocal/options.unfr0zen-new\n\
                       new etc/added/file.unfr0zen-new\n");
    unit.put(JOURNAL, &journal(1, "reboot", "swapping"));
    unit.guard(600);
    unit.assert_rolled_back();
    assert!(!unit.path(STAGING).exists());
    assert!(unit.logged().ends_with("-t nestlocal update rolled back\n"), "{}", unit.logged());
    assert_eq!(std::fs::read_to_string(unit.rebooted()).unwrap(), "reboot\n");
    assert!(!unit.restarted().exists() && unit.watcher().is_none());

    // A set that only restarts nestlocal starts the old one, as this start
    // does anyway.
    let unit = Unit::swapped("swappingrestart", "restart", false);
    unit.put(JOURNAL, &journal(1, "restart", "swapping"));
    unit.guard(600);
    unit.assert_rolled_back();
    assert!(!unit.restarted().exists() && !unit.rebooted().exists());
}

#[test]
fn a_kept_set_without_marker_is_cleaned_up() {
    // nestlocal kept the set and stopped before its backups went.
    let unit = Unit::swapped("kept", "reboot", false);
    unit.guard(600);
    unit.assert_kept();
    assert!(!unit.restarted().exists() && !unit.rebooted().exists());
    assert!(unit.watcher().is_none());
}

#[test]
fn an_unknown_journal_version_is_left_alone() {
    let unit = Unit::swapped("version", "restart", true);
    unit.put(JOURNAL, &journal(2, "restart", "swapping"));
    unit.guard(1);
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(unit.binary(), "new");
    assert_eq!(unit.get(JOURNAL), Some(journal(2, "restart", "swapping")));
    assert!(unit.path("nestlabs/sbin/nestlocal.unfr0zen-previous").exists());
    assert_eq!(std::fs::read_to_string(unit.marker()).unwrap(), "0\n");
    assert!(unit.watcher().is_none());
    assert!(!unit.restarted().exists() && !unit.rebooted().exists());
    assert!(unit.logged().contains("unfr0zen-update 2"), "{}", unit.logged());
}

#[test]
fn leftover_staged_files_are_removed() {
    // An apply stopped while staging: no journal yet.
    let unit = Unit::new("staged");
    std::fs::create_dir_all(unit.path("etc/added/deeper")).unwrap();
    unit.put("etc/nestlocal/options.unfr0zen-new", "new");
    unit.put("etc/added/deeper/file.unfr0zen-new", "new");
    // Only staged names go, whatever the list says.
    unit.put(STAGING, "new etc/nestlocal/options.unfr0zen-new\nmkdir etc/added\nmkdir etc/added/deeper\n\
                       new etc/added/deeper/file.unfr0zen-new\nnew etc/monitrc\n");
    unit.guard(600);
    assert!(!unit.path("etc/nestlocal/options.unfr0zen-new").exists());
    assert!(!unit.path("etc/added").exists());
    assert!(!unit.path(STAGING).exists());
    assert_eq!(unit.get("etc/nestlocal/options").as_deref(), Some("old"));
    assert_eq!(unit.get("etc/monitrc").as_deref(), Some("old"));
    assert!(!unit.restarted().exists() && !unit.rebooted().exists());
}

#[test]
fn a_commit_before_the_watch_keeps_the_update() {
    let unit = Unit::swapped("commit", "reboot", true);
    unit.guard(2);
    std::thread::sleep(Duration::from_millis(500));
    unit.keep();
    std::thread::sleep(Duration::from_secs(3));
    unit.assert_kept();
    assert!(!unit.restarted().exists() && !unit.rebooted().exists());
    assert!(unit.watcher().is_none(), "the watcher ends with the commit");
    assert_eq!(unit.logged(), "");
}

#[test]
fn a_newer_update_gets_a_whole_watch_of_its_own() {
    let unit = Unit::swapped("newer", "restart", true);
    unit.guard(3);
    std::thread::sleep(Duration::from_millis(1500));
    // Kept, and the next set swapped in before the first watcher looked.
    unit.keep();
    let newer = Unit::swapped("newer-next", "restart", true);
    for name in ["nestlabs/sbin/nestlocal.unfr0zen-previous", "etc/nestlocal/options.unfr0zen-previous",
                 "etc/monitrc.unfr0zen-previous", JOURNAL] {
        std::fs::copy(newer.path(name), unit.path(name)).unwrap();
    }
    std::fs::write(unit.marker(), "0\n").unwrap();
    unit.put("nestlabs/sbin/nestlocal", "newer");
    unit.guard(3);
    // Past the first update's watch, within the newer one's.
    std::thread::sleep(Duration::from_millis(2000));
    assert_eq!(unit.binary(), "newer");
    assert!(!unit.restarted().exists(), "the first update's watch rolled back the newer one");
    assert!(until(WATCHED, || unit.restarted().exists()), "the newer update was never watched");
    assert_eq!(unit.binary(), "old");
    assert_eq!(unit.restarts(), 1);
}

#[test]
fn starts_at_once_each_count_and_share_one_watcher() {
    let unit = Unit::swapped("together", "restart", true);
    let mut both = [unit.sh(3, &[GUARD, "start"]).spawn().unwrap(), unit.sh(3, &[GUARD, "start"]).spawn().unwrap()];
    assert!(both.iter_mut().all(|guard| guard.wait().unwrap().success()));
    assert_eq!(std::fs::read_to_string(unit.marker()).unwrap(), "2\n");
    assert!(until(WATCHED, || unit.restarted().exists()), "the watch never restarted nestlocal");
    std::thread::sleep(Duration::from_millis(1500));
    unit.assert_rolled_back();
    assert_eq!(unit.restarts(), 1, "more than one watcher rolled back");
}

#[test]
fn a_marker_without_a_journal_is_dropped() {
    let unit = Unit::new("nojournal");
    std::fs::write(unit.marker(), "0\n").unwrap();
    unit.guard(1);
    assert!(!unit.marker().exists());
    assert_eq!(unit.binary(), "old");
    assert!(unit.watcher().is_none());
    assert!(!unit.restarted().exists());
    assert_eq!(unit.logged(), "-t nestlocal update marker without a journal dropped\n");
}

// Another process's pid in the pid file, as when a watcher died without
// removing it and its pid was reused, is told apart through /proc.
#[cfg(target_os = "linux")]
#[test]
fn a_reused_pid_in_the_pid_file_is_not_a_watcher() {
    let unit = Unit::swapped("reused", "restart", true);
    let mut other = Command::new("sleep").arg("30").spawn().unwrap();
    std::fs::write(unit.root.join("var/run/nestlocal-update-guard.pid"), format!("{}\n", other.id())).unwrap();
    unit.guard(1);
    let rolled_back = until(WATCHED, || unit.restarted().exists());
    let _ = other.kill();
    let _ = other.wait();
    assert!(rolled_back, "no watch ran");
    unit.assert_rolled_back();
}

#[test]
fn a_rollback_restarts_nestlocal_whatever_the_logger_does() {
    let unit = Unit::swapped("nologger", "restart", true).without_logger();
    unit.guard(1);
    assert!(until(WATCHED, || unit.restarted().exists()), "the watch never restarted nestlocal");
    unit.assert_rolled_back();
}

#[test]
fn a_rollback_that_cannot_put_the_binary_back_is_finished_by_the_next_start() {
    let unit = Unit::swapped("stuck", "restart", true);
    // A directory cannot replace the binary, whoever runs the test.
    let previous = unit.path("nestlabs/sbin/nestlocal.unfr0zen-previous");
    std::fs::remove_file(&previous).unwrap();
    std::fs::create_dir(&previous).unwrap();
    unit.guard(1);
    assert!(until(WATCHED, || unit.logged().contains("could not be put back")), "the watch said nothing");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(unit.binary(), "new");
    assert!(unit.marker().exists(), "the next start retries");
    // Journaled as part way out, so neither a start's trial nor nestlocal
    // keeps what is left of the set.
    assert_eq!(unit.get(JOURNAL), Some(journal(1, "restart", "swapping")));
    assert!(!unit.restarted().exists() && !unit.rolled_back().exists());
    std::fs::remove_dir(&previous).unwrap();
    unit.put("nestlabs/sbin/nestlocal.unfr0zen-previous", "old");
    unit.guard(600);
    unit.assert_rolled_back();
    assert!(unit.watcher().is_none());
    assert!(!unit.restarted().exists() && !unit.rebooted().exists());
}

#[test]
fn a_rollback_cut_short_is_finished_by_the_next_start() {
    // The watch's rollback stops, as at a power cut, after its second move:
    // nestlocal is still the set's.
    let mut unit = Unit::swapped("cut", "restart", true).cut_after(2);
    unit.guard(1);
    assert!(until(WATCHED, || unit.get("etc/monitrc").is_some()), "the watch never began the rollback");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(unit.binary(), "new");
    assert!(unit.marker().exists() && unit.path(JOURNAL).exists());
    assert!(!unit.restarted().exists() && !unit.rolled_back().exists());
    // The lock the stopped guard held is taken over after 5 s; spare the wait.
    std::fs::remove_dir(unit.root.join("var/run/nestlocal-update-guard.lock")).unwrap();
    unit.moves = None;
    unit.guard(600);
    unit.assert_rolled_back();
    assert!(unit.watcher().is_none(), "the rest of the set got a trial of its own");
    assert!(!unit.restarted().exists() && !unit.rebooted().exists());
}

#[test]
fn a_lock_left_by_a_killed_guard_is_taken_over() {
    let unit = Unit::swapped("stalelock", "restart", true);
    let lock = unit.root.join("var/run/nestlocal-update-guard.lock");
    std::fs::create_dir(&lock).unwrap();
    let started = Instant::now();
    unit.guard(600);
    assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
    assert_eq!(std::fs::read_to_string(unit.marker()).unwrap(), "1\n");
    assert!(!lock.exists());
}
