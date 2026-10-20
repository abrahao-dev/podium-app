//! Git worktree operations for the mission merge queue, plus path/glob helpers.
//!
//! Pure library code: no Tauri types. Every git call runs through
//! `crate::pty::hidden_command` so no console window flashes on Windows.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Paths compare case-insensitively on Windows only.
const CASE_INSENSITIVE_PATHS: bool = cfg!(windows);

fn git_command(dir: &Path) -> Command {
    let mut cmd = crate::pty::hidden_command("git");
    cmd.arg("-C")
        .arg(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    cmd
}

fn git_raw(dir: &Path, args: &[&str]) -> Result<Output, String> {
    git_command(dir)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))
}

fn error_text(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return stdout;
    }
    format!("git exited with {:?}", output.status.code())
}

/// Runs git and returns trimmed stdout, or trimmed stderr as the error.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_raw(dir, args)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(error_text(&output))
    }
}

fn path_arg(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn non_empty_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn repo_root(dir: &Path) -> Result<PathBuf, String> {
    git(dir, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

pub fn head_commit(repo: &Path) -> Result<String, String> {
    git(repo, &["rev-parse", "HEAD"])
}

pub fn current_branch(repo: &Path) -> Result<Option<String>, String> {
    let output = git_raw(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    if output.status.success() {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok(if name.is_empty() { None } else { Some(name) });
    }
    // Exit 1 with --quiet means HEAD is detached; anything else is a real error.
    if output.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(error_text(&output))
    }
}

pub fn add_worktree(repo: &Path, dir: &Path, branch: &str, from: &str) -> Result<(), String> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    git(
        repo,
        &["worktree", "add", "-b", branch, path_arg(dir)?, from],
    )
    .map(|_| ())
}

pub fn remove_worktree(repo: &Path, dir: &Path, delete_branch: Option<&str>) -> Result<(), String> {
    if dir.exists() {
        if let Err(err) = git(repo, &["worktree", "remove", "--force", path_arg(dir)?]) {
            if !err.contains("is not a working tree") {
                return Err(err);
            }
        }
    }
    git(repo, &["worktree", "prune"])?;
    if let Some(branch) = delete_branch {
        if let Err(err) = git(repo, &["branch", "-D", branch]) {
            if !err.contains("not found") {
                return Err(err);
            }
        }
    }
    Ok(())
}

pub fn commit_all(dir: &Path, message: &str) -> Result<bool, String> {
    git(dir, &["add", "-A"])?;
    if git(dir, &["status", "--porcelain"])?.is_empty() {
        return Ok(false);
    }
    git(dir, &["commit", "--no-verify", "-q", "-m", message])?;
    Ok(true)
}

pub fn commits_ahead(repo: &Path, base: &str, branch: &str) -> Result<u32, String> {
    let range = format!("{base}..{branch}");
    let count = git(repo, &["rev-list", "--count", &range])?;
    count
        .parse()
        .map_err(|e| format!("unexpected rev-list output {count:?}: {e}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Merged { commit: String },
    UpToDate,
    Conflict { files: Vec<String> },
}

pub fn merge_branch(worktree: &Path, branch: &str, message: &str) -> Result<MergeOutcome, String> {
    let before = head_commit(worktree)?;
    let output = git_raw(
        worktree,
        &["merge", "--no-ff", "--no-edit", "-m", message, branch],
    )?;
    if output.status.success() {
        let after = head_commit(worktree)?;
        return Ok(if after == before {
            MergeOutcome::UpToDate
        } else {
            MergeOutcome::Merged { commit: after }
        });
    }
    let files = non_empty_lines(&git(
        worktree,
        &["diff", "--name-only", "--diff-filter=U"],
    )?);
    let merge_error = error_text(&output);
    // Leave the worktree clean whatever happened; ignore "no merge to abort".
    let _ = git_raw(worktree, &["merge", "--abort"]);
    if files.is_empty() {
        Err(merge_error)
    } else {
        Ok(MergeOutcome::Conflict { files })
    }
}

pub fn undo_merge(worktree: &Path, commit: &str) -> Result<(), String> {
    let head = head_commit(worktree)?;
    if head != commit {
        return Err(format!(
            "HEAD moved since the merge (expected {commit}, found {head}); not undoing"
        ));
    }
    git(worktree, &["reset", "--hard", "-q", "HEAD~1"]).map(|_| ())
}

pub fn conflicting_files(repo: &Path, a: &str, b: &str) -> Result<Vec<String>, String> {
    let output = git_raw(
        repo,
        &["merge-tree", "--write-tree", "--name-only", "--no-messages", a, b],
    )?;
    match output.status.code() {
        Some(0) => Ok(Vec::new()),
        Some(1) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut files: Vec<String> = non_empty_lines(&stdout).into_iter().skip(1).collect();
            files.dedup();
            Ok(files)
        }
        _ => {
            let err = error_text(&output);
            let lower = err.to_lowercase();
            if lower.contains("unknown option") || lower.contains("usage:") {
                Ok(Vec::new())
            } else {
                Err(err)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRun {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub output_tail: String,
}

fn spawn_reader(
    mut source: impl Read + Send + 'static,
    buffer: Arc<Mutex<Vec<u8>>>,
    done: mpsc::Sender<()>,
) {
    thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match source.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut buf) = buffer.lock() {
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
        let _ = done.send(());
    });
}

fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

pub fn run_with_timeout(mut cmd: Command, timeout: Duration, tail_lines_count: usize) -> CommandRun {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return CommandRun {
                success: false,
                exit_code: None,
                timed_out: false,
                output_tail: format!("failed to start command: {e}"),
            }
        }
    };

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let (done_tx, done_rx) = mpsc::channel();
    let mut readers = 0;
    if let Some(stdout) = child.stdout.take() {
        spawn_reader(stdout, Arc::clone(&buffer), done_tx.clone());
        readers += 1;
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_reader(stderr, Arc::clone(&buffer), done_tx.clone());
        readers += 1;
    }
    drop(done_tx);

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait().ok();
                }
                thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break None,
        }
    };

    // Grandchildren may keep the pipes open after a kill; don't wait on them forever.
    let drain_deadline = Instant::now()
        + if timed_out {
            Duration::from_millis(500)
        } else {
            Duration::from_secs(5)
        };
    for _ in 0..readers {
        let remaining = drain_deadline.saturating_duration_since(Instant::now());
        if done_rx.recv_timeout(remaining).is_err() {
            break;
        }
    }

    let text = buffer
        .lock()
        .map(|buf| String::from_utf8_lossy(&buf).into_owned())
        .unwrap_or_default();
    let exit_code = status.and_then(|s| s.code());
    CommandRun {
        success: !timed_out && status.is_some_and(|s| s.success()),
        exit_code,
        timed_out,
        output_tail: tail_lines(&text, tail_lines_count),
    }
}

/// Normalizes a path or pattern into '/'-separated, non-empty segments.
fn segments(input: &str) -> Vec<String> {
    let normalized = input.replace('\\', "/");
    let normalized = if CASE_INSENSITIVE_PATHS {
        normalized.to_lowercase()
    } else {
        normalized
    };
    let mut rest = normalized.as_str();
    while let Some(stripped) = rest.strip_prefix("./") {
        rest = stripped;
    }
    rest.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .map(str::to_string)
        .collect()
}

fn has_wildcard(segment: &str) -> bool {
    segment.contains(['*', '?'])
}

/// Matches one segment against a pattern segment with `*` and `?`.
fn segment_match(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

fn segments_match(pattern: &[String], path: &[String]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((first, rest)) if first == "**" => {
            (0..=path.len()).any(|skip| segments_match(rest, &path[skip..]))
        }
        Some((first, rest)) => match path.split_first() {
            None => false,
            Some((seg, path_rest)) => {
                let p: Vec<char> = first.chars().collect();
                let t: Vec<char> = seg.chars().collect();
                segment_match(&p, &t) && segments_match(rest, path_rest)
            }
        },
    }
}

fn is_segment_prefix(prefix: &[String], path: &[String]) -> bool {
    prefix.len() <= path.len() && prefix.iter().zip(path).all(|(a, b)| a == b)
}

pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern = segments(pattern);
    let path = segments(path);
    if pattern.iter().any(|s| has_wildcard(s)) {
        segments_match(&pattern, &path)
    } else {
        // A literal pattern names a file or a directory: match it and everything under it.
        is_segment_prefix(&pattern, &path)
    }
}

/// Path segments up to (not including) the first segment containing a wildcard.
fn literal_prefix(pattern: &str) -> Vec<String> {
    segments(pattern)
        .into_iter()
        .take_while(|s| !has_wildcard(s))
        .collect()
}

fn patterns_overlap(a: &str, b: &str) -> bool {
    let prefix_a = literal_prefix(a);
    let prefix_b = literal_prefix(b);
    if is_segment_prefix(&prefix_a, &prefix_b) || is_segment_prefix(&prefix_b, &prefix_a) {
        return true;
    }
    glob_match(a, &prefix_b.join("/")) || glob_match(b, &prefix_a.join("/"))
}

pub fn scopes_overlap(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() {
        return true;
    }
    a.iter()
        .any(|pa| b.iter().any(|pb| patterns_overlap(pa, pb)))
}

fn comparable_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text
        .strip_prefix("//?/UNC/")
        .map(|rest| format!("//{rest}"))
        .unwrap_or_else(|| text.strip_prefix("//?/").unwrap_or(&text).to_string());
    text.trim_end_matches('/').to_string()
}

pub fn relative_to(root: &Path, path: &Path) -> Option<String> {
    let (root, path) = match (root.canonicalize(), path.canonicalize()) {
        (Ok(r), Ok(p)) => (r, p),
        _ => (root.to_path_buf(), path.to_path_buf()),
    };
    let root_text = comparable_path(&root);
    let path_text = comparable_path(&path);
    let (root_cmp, path_cmp) = if CASE_INSENSITIVE_PATHS {
        (root_text.to_lowercase(), path_text.to_lowercase())
    } else {
        (root_text.clone(), path_text.clone())
    };
    if path_cmp == root_cmp {
        return Some(String::new());
    }
    let root_with_sep = format!("{root_cmp}/");
    if !path_cmp.starts_with(&root_with_sep) {
        return None;
    }
    // Lowercasing can change byte lengths for some characters; slice by char count.
    let skip = root_with_sep.chars().count();
    Some(path_text.chars().skip(skip).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn glob_match_cases() {
        assert!(glob_match("src/lib", "src/lib/a.ts"));
        assert!(glob_match("src/lib", "src/lib"));
        assert!(!glob_match("src/lib", "src/library/a.ts"));
        assert!(glob_match("src/*.ts", "src/a.ts"));
        assert!(!glob_match("src/*.ts", "src/sub/a.ts"));
        assert!(glob_match("src/**/*.ts", "src/a.ts"));
        assert!(glob_match("src/**/*.ts", "src/x/y/a.ts"));
        assert!(glob_match("**", "anything/at/all"));
        assert!(glob_match("**/*.rs", "src-tauri/src/pty.rs"));
        assert!(glob_match("src/?.ts", "src/a.ts"));
        assert!(!glob_match("src/?.ts", "src/ab.ts"));
        assert!(glob_match("./src\\lib", "src/lib/x"));
        assert!(glob_match("src/**", "src/a/b"));
        assert!(!glob_match("docs/**", "src/a"));
        assert_eq!(glob_match("SRC/Lib", "src/lib/a.ts"), cfg!(windows));
    }

    #[test]
    fn scopes_overlap_cases() {
        assert!(scopes_overlap(&[], &strings(&["src"])));
        assert!(scopes_overlap(&strings(&["src"]), &strings(&["src/lib/a.ts"])));
        assert!(scopes_overlap(&strings(&["src/**"]), &strings(&["src/lib/*.ts"])));
        assert!(scopes_overlap(&strings(&["**/*.ts"]), &strings(&["docs"])));
        assert!(scopes_overlap(&strings(&["src/*.ts"]), &strings(&["src/a.ts"])));
        assert!(!scopes_overlap(&strings(&["src/**"]), &strings(&["docs/**"])));
        assert!(!scopes_overlap(&strings(&["src/a.ts"]), &strings(&["src/b.ts"])));
        assert!(!scopes_overlap(
            &strings(&["src/lib", "docs"]),
            &strings(&["src-tauri/**", "README.md"])
        ));
    }

    #[test]
    fn relative_to_cases() {
        let root = Path::new("/work/repo");
        assert_eq!(
            relative_to(root, Path::new("/work/repo/src/a.ts")).as_deref(),
            Some("src/a.ts")
        );
        assert_eq!(relative_to(root, Path::new("/work/repo")).as_deref(), Some(""));
        assert_eq!(relative_to(root, Path::new("/work/repository/a")), None);
        assert_eq!(relative_to(root, Path::new("/elsewhere")), None);
        assert_eq!(
            relative_to(root, Path::new("/work/REPO/x")).is_some(),
            cfg!(windows)
        );

        let tmp = std::env::temp_dir();
        assert_eq!(relative_to(&tmp, &tmp.join("a").join("b")).as_deref(), Some("a/b"));
    }

    struct TempRoot(PathBuf);

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn unique_temp_root(tag: &str) -> TempRoot {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "podium-worktree-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp root");
        TempRoot(dir)
    }

    fn write(path: &Path, text: &str) {
        std::fs::write(path, text).expect("write file");
    }

    #[test]
    fn worktree_merge_queue_flow() {
        let temp = unique_temp_root("flow");
        let repo = temp.0.join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        git(&repo, &["init", "-q", "-b", "main"]).unwrap();
        git(&repo, &["config", "user.name", "Podium Test"]).unwrap();
        git(&repo, &["config", "user.email", "test@podium.invalid"]).unwrap();
        git(&repo, &["config", "core.autocrlf", "false"]).unwrap();
        git(&repo, &["config", "commit.gpgsign", "false"]).unwrap();
        write(&repo.join("a.txt"), "alpha\n");
        write(&repo.join("b.txt"), "beta\n");
        assert!(commit_all(&repo, "initial").unwrap());
        assert!(!commit_all(&repo, "nothing").unwrap());

        let root = repo_root(&repo).unwrap();
        assert_eq!(relative_to(&root, &repo.join("a.txt")).as_deref(), Some("a.txt"));
        assert_eq!(current_branch(&repo).unwrap().as_deref(), Some("main"));

        let wt = temp.0.join("wt");
        let integration = wt.join("integration");
        let m1 = wt.join("m1");
        let m2 = wt.join("m2");
        add_worktree(&repo, &integration, "integration", "main").unwrap();
        add_worktree(&repo, &m1, "mission/m1", "main").unwrap();
        add_worktree(&repo, &m2, "mission/m2", "main").unwrap();

        write(&m1.join("a.txt"), "alpha from m1\n");
        write(&m2.join("b.txt"), "beta from m2\n");
        assert!(commit_all(&m1, "m1 work").unwrap());
        assert!(commit_all(&m2, "m2 work").unwrap());
        assert_eq!(commits_ahead(&repo, "main", "mission/m1").unwrap(), 1);
        assert!(conflicting_files(&repo, "mission/m1", "mission/m2")
            .unwrap()
            .is_empty());

        let first = merge_branch(&integration, "mission/m1", "merge m1").unwrap();
        assert!(matches!(first, MergeOutcome::Merged { .. }));
        let second = merge_branch(&integration, "mission/m2", "merge m2").unwrap();
        assert!(matches!(second, MergeOutcome::Merged { .. }));
        assert_eq!(
            merge_branch(&integration, "mission/m2", "again").unwrap(),
            MergeOutcome::UpToDate
        );
        assert_eq!(
            std::fs::read_to_string(integration.join("b.txt")).unwrap(),
            "beta from m2\n"
        );

        // Two missions editing the same line.
        let c1 = wt.join("c1");
        let c2 = wt.join("c2");
        add_worktree(&repo, &c1, "mission/c1", "integration").unwrap();
        add_worktree(&repo, &c2, "mission/c2", "integration").unwrap();
        write(&c1.join("a.txt"), "c1 line\n");
        write(&c2.join("a.txt"), "c2 line\n");
        assert!(commit_all(&c1, "c1 work").unwrap());
        assert!(commit_all(&c2, "c2 work").unwrap());
        assert_eq!(
            conflicting_files(&repo, "mission/c1", "mission/c2").unwrap(),
            vec!["a.txt".to_string()]
        );

        let before_c1 = head_commit(&integration).unwrap();
        let merged = match merge_branch(&integration, "mission/c1", "merge c1").unwrap() {
            MergeOutcome::Merged { commit } => commit,
            other => panic!("expected merge, got {other:?}"),
        };
        match merge_branch(&integration, "mission/c2", "merge c2").unwrap() {
            MergeOutcome::Conflict { files } => assert_eq!(files, vec!["a.txt".to_string()]),
            other => panic!("expected conflict, got {other:?}"),
        }
        assert!(git(&integration, &["status", "--porcelain"]).unwrap().is_empty());
        assert_eq!(head_commit(&integration).unwrap(), merged);

        assert!(undo_merge(&integration, "0000000").is_err());
        undo_merge(&integration, &merged).unwrap();
        assert_eq!(head_commit(&integration).unwrap(), before_c1);

        // Detached HEAD reports no branch.
        git(&c1, &["checkout", "-q", "--detach"]).unwrap();
        assert_eq!(current_branch(&c1).unwrap(), None);

        for (dir, branch) in [
            (&m1, "mission/m1"),
            (&m2, "mission/m2"),
            (&c1, "mission/c1"),
            (&c2, "mission/c2"),
            (&integration, "integration"),
        ] {
            remove_worktree(&repo, dir, Some(branch)).unwrap();
            assert!(!dir.exists());
        }
        // Already removed: tolerated.
        remove_worktree(&repo, &m1, None).unwrap();
        assert!(git(&repo, &["branch", "--list", "mission/*"]).unwrap().is_empty());
    }

    #[test]
    fn run_with_timeout_kills_slow_command() {
        #[cfg(windows)]
        let slow = "ping -n 5 127.0.0.1 >NUL";
        #[cfg(not(windows))]
        let slow = "sleep 5";
        let started = Instant::now();
        let run = run_with_timeout(
            crate::pty::shell_exec(slow),
            Duration::from_millis(400),
            10,
        );
        assert!(run.timed_out);
        assert!(!run.success);
        assert!(started.elapsed() < Duration::from_secs(3));

        let quick = run_with_timeout(
            crate::pty::shell_exec("echo one&& echo two&& echo three"),
            Duration::from_secs(10),
            2,
        );
        assert!(quick.success, "{quick:?}");
        assert_eq!(quick.exit_code, Some(0));
        assert!(!quick.timed_out);
        assert_eq!(quick.output_tail.replace('\r', ""), "two\nthree");
    }
}
