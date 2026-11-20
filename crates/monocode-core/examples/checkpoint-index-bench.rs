use std::fs::{self, FileTimes};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use monocode_core::checkpoint::Store;

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(output.stdout)
}

fn set_past_mtime(path: &Path) -> Result<(), String> {
    let modified = SystemTime::now()
        .checked_sub(Duration::from_secs(4 * 60 * 60))
        .ok_or_else(|| "clock predates benchmark window".to_string())?;
    std::fs::File::options()
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?
        .set_times(FileTimes::new().set_modified(modified))
        .map_err(|e| e.to_string())
}

fn counter_value(path: &Path) -> usize {
    fs::read_to_string(path)
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

fn reset_counter(path: &Path) -> Result<(), String> {
    fs::write(path, "").map_err(|e| e.to_string())
}

fn clear_indexes(indexes: &Path) -> Result<(), String> {
    match fs::remove_dir_all(indexes) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn build_fixture() -> Result<(tempfile::TempDir, PathBuf, PathBuf, PathBuf), String> {
    let temp = tempfile::Builder::new()
        .prefix("monocode-checkpoint-bench-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let root = temp.path().to_path_buf();

    let repo = root.join("repo");
    fs::create_dir_all(&repo).map_err(|e| e.to_string())?;
    git(&repo, &["init"])?;
    git(&repo, &["config", "user.name", "Bench"])?;
    git(&repo, &["config", "user.email", "bench@example.invalid"])?;
    git(&repo, &["config", "core.autocrlf", "false"])?;
    git(&repo, &["config", "commit.gpgsign", "false"])?;

    let counter = root.join("clean-count");
    let clean = format!("sh -c 'echo tick >> {}; cat'", counter.display());
    git(&repo, &["config", "filter.count.clean", &clean])?;
    git(&repo, &["config", "filter.count.required", "true"])?;
    fs::write(repo.join(".gitattributes"), "*.big filter=count\n").map_err(|e| e.to_string())?;

    fs::write(repo.join("a.txt"), "original\n").map_err(|e| e.to_string())?;
    for i in 0..240 {
        let dir = repo.join("src").join(format!("group-{:02}", i % 12));
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        fs::write(dir.join(format!("file-{i:04}.txt")), format!("line {i}\n"))
            .map_err(|e| e.to_string())?;
    }
    for i in 0..24 {
        let path = repo.join(format!("blob-{i:02}.big"));
        fs::write(&path, "x".repeat(96 * 1024)).map_err(|e| e.to_string())?;
        set_past_mtime(&path)?;
    }

    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-m", "initial"])?;
    reset_counter(&counter)?;

    Ok((temp, root, repo, counter))
}

fn run_turn(
    store: &Store,
    cwd: &str,
    repo: &Path,
    indexes: &Path,
    session: &str,
    turn: &str,
    reuse_index: bool,
) -> Result<(), String> {
    if !reuse_index {
        clear_indexes(indexes)?;
    }
    store.begin_turn(session, cwd, turn)?;
    fs::write(repo.join("a.txt"), format!("original\n{turn}\n")).map_err(|e| e.to_string())?;
    if !reuse_index {
        clear_indexes(indexes)?;
    }
    store.finish_turn(session, cwd, turn)
}

fn median(values: &mut [Duration]) -> Duration {
    values.sort();
    values[values.len() / 2]
}

fn run_case(label: &str, reuse_index: bool, turns: usize) -> Result<(), String> {
    let (_temp, root, repo, counter) = build_fixture()?;
    let store = Store::new(root.join("checkpoints"));
    let indexes = root.join("checkpoints/indexes");
    let cwd = repo
        .to_str()
        .ok_or_else(|| "non-UTF-8 repo path".to_string())?;

    run_turn(
        &store,
        cwd,
        &repo,
        &indexes,
        "warmup",
        "warmup",
        reuse_index,
    )?;
    reset_counter(&counter)?;
    let ticks_before = counter_value(&counter);

    let mut timings = Vec::with_capacity(turns);
    for i in 0..turns {
        let session = format!("session-{i}");
        let turn = format!("turn-{i}");
        let started = Instant::now();
        run_turn(&store, cwd, &repo, &indexes, &session, &turn, reuse_index)?;
        timings.push(started.elapsed());
    }
    let ticks = counter_value(&counter) - ticks_before;
    let median = median(&mut timings);
    let total: Duration = timings.iter().copied().sum();

    println!(
        "{label}: {turns} turns, median {:.1} ms, mean {:.1} ms, total {:.2} s, clean-filter calls {ticks}",
        median.as_secs_f64() * 1000.0,
        total.as_secs_f64() * 1000.0 / turns as f64,
        total.as_secs_f64()
    );
    Ok(())
}

fn main() -> Result<(), String> {
    let turns = std::env::args()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .filter(|value: &usize| *value > 0)
        .unwrap_or(8);

    run_case("fresh index", false, turns)?;
    run_case("reused index", true, turns)?;
    Ok(())
}
