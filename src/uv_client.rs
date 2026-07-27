use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur when interacting with `uv`.
#[derive(Debug)]
pub enum UvError {
    /// The `uv` binary was not found on the system PATH.
    UvNotFound,
    /// A `uv` command exited with a non-zero status or could not be spawned for
    /// any reason other than the binary being absent.
    CommandFailed(String),
}

impl fmt::Display for UvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UvError::UvNotFound => write!(
                f,
                "uv is not installed or not on PATH. \
                 Please install uv (https://docs.astral.sh/uv/getting-started/installation/) \
                 to use this feature."
            ),
            UvError::CommandFailed(msg) => write!(f, "uv command failed: {}", msg),
        }
    }
}

impl std::error::Error for UvError {}

// ── Environment directory ─────────────────────────────────────────────────────

/// Returns the imhottep config directory: the parent directory of the request
/// library file (`$XDG_CONFIG_HOME/imhottep/` or `~/.config/imhottep/`).
///
/// An optional `base_override` (used in tests) replaces the XDG/HOME lookup.
fn env_dir_with_base(base_override: Option<&Path>) -> PathBuf {
    let base = match base_override {
        Some(provided_base) => provided_base.to_path_buf(),
        None => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| {
                    let mut p = PathBuf::from(h);
                    p.push(".config");
                    p
                })
            })
            .unwrap_or_else(|| PathBuf::from(".")),
    };

    base.join("imhottep")
}

/// Returns the imhottep config directory using the standard XDG/HOME lookup.
pub fn env_dir() -> PathBuf {
    env_dir_with_base(None)
}

// ── Core subprocess helper ────────────────────────────────────────────────────

/// Spawn `uv <args>` with `working_dir` as the current directory and wait for
/// it to finish.
///
/// * If the binary is not found → `UvError::UvNotFound`.
/// * If the process exits with a non-zero status → `UvError::CommandFailed`
///   containing the trimmed stderr output.
/// * On success → the trimmed stdout output as a `String`.
fn run_uv(args: &[&str], working_dir: &Path) -> Result<String, UvError> {
    let output = Command::new("uv")
        .args(args)
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                UvError::UvNotFound
            } else {
                UvError::CommandFailed(e.to_string())
            }
        })?
        .wait_with_output()
        .map_err(|e| UvError::CommandFailed(e.to_string()))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        Err(UvError::CommandFailed(detail))
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Add a Python package to the shared imhottep virtual environment.
///
/// Runs `uv pip install <package> --project <env_dir>`.
/// Returns the stdout from `uv` on success.
///
/// # Errors
/// Returns `UvError::UvNotFound` if `uv` is not on PATH, or
/// `UvError::CommandFailed` if the install fails.
pub fn add_dependency(package: &str) -> Result<String, UvError> {
    add_dependency_with_base(package, None)
}

fn add_dependency_with_base(package: &str, base: Option<&Path>) -> Result<String, UvError> {
    let dir = env_dir_with_base(base);
    run_uv(
        &[
            "pip",
            "install",
            package,
            "--project",
            dir.to_string_lossy().as_ref(),
        ],
        &dir,
    )
}

/// Remove a Python package from the shared imhottep virtual environment.
///
/// Runs `uv pip uninstall <package> --project <env_dir>`.
/// Returns the stdout from `uv` on success.
///
/// # Errors
/// Returns `UvError::UvNotFound` if `uv` is not on PATH, or
/// `UvError::CommandFailed` if the uninstall fails.
pub fn remove_dependency(package: &str) -> Result<String, UvError> {
    remove_dependency_with_base(package, None)
}

fn remove_dependency_with_base(package: &str, base: Option<&Path>) -> Result<String, UvError> {
    let dir = env_dir_with_base(base);
    run_uv(
        &[
            "pip",
            "uninstall",
            package,
            "--project",
            dir.to_string_lossy().as_ref(),
        ],
        &dir,
    )
}

/// List all packages installed in the shared imhottep virtual environment.
///
/// Runs `uv pip list --project <env_dir>`.
/// Returns the stdout from `uv` (one `name version` entry per line) on success.
///
/// # Errors
/// Returns `UvError::UvNotFound` if `uv` is not on PATH, or
/// `UvError::CommandFailed` if the command fails (e.g., the environment has not
/// been initialized yet).
pub fn list_dependencies() -> Result<String, UvError> {
    list_dependencies_with_base(None)
}

fn list_dependencies_with_base(base: Option<&Path>) -> Result<String, UvError> {
    let dir = env_dir_with_base(base);
    run_uv(
        &["pip", "list", "--project", dir.to_string_lossy().as_ref()],
        &dir,
    )
}

/// Reinitialize the shared imhottep virtual environment from scratch.
///
/// Removes the existing `.venv` directory (if present), then runs
/// `uv venv --project <env_dir>` to create a fresh environment.
/// Returns the stdout from `uv` on success.
///
/// # Errors
/// Returns `UvError::UvNotFound` if `uv` is not on PATH, or
/// `UvError::CommandFailed` if environment creation fails.
pub fn reinit_environment() -> Result<String, UvError> {
    reinit_environment_with_base(None)
}

fn reinit_environment_with_base(base: Option<&Path>) -> Result<String, UvError> {
    let dir = env_dir_with_base(base);
    let venv_path = dir.join(".venv");
    if venv_path.exists() {
        std::fs::remove_dir_all(&venv_path)
            .map_err(|e| UvError::CommandFailed(format!("Failed to remove .venv: {e}")))?;
    }
    run_uv(&["venv", "--project", dir.to_string_lossy().as_ref()], &dir)
}

/// Run a string of Python code inside the shared imhottep virtual environment.
///
/// Runs `uv run python -c <code> --project <env_dir>`.
/// Returns the combined stdout output on success.
///
/// # Errors
/// Returns `UvError::UvNotFound` if `uv` is not on PATH, or
/// `UvError::CommandFailed` if the script exits with a non-zero status (stderr
/// is included in the error message).
pub fn run_code(code: &str) -> Result<String, UvError> {
    run_code_with_base(code, None)
}

fn run_code_with_base(code: &str, base: Option<&Path>) -> Result<String, UvError> {
    let dir = env_dir_with_base(base);
    run_uv(
        &[
            "run",
            "python",
            "-c",
            code,
            "--project",
            dir.to_string_lossy().as_ref(),
        ],
        &dir,
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    // ── UvError display ───────────────────────────────────────────────────────

    #[test]
    fn uv_not_found_display_mentions_installation() {
        let msg = UvError::UvNotFound.to_string();
        assert!(msg.contains("uv is not installed"), "got: {msg}");
        assert!(msg.contains("https://"), "got: {msg}");
    }

    #[test]
    fn command_failed_display_includes_message() {
        let msg = UvError::CommandFailed("exit code 1".to_string()).to_string();
        assert!(msg.contains("exit code 1"), "got: {msg}");
    }

    #[test]
    fn uv_error_implements_std_error() {
        fn accepts_error(_: &dyn std::error::Error) {}
        accepts_error(&UvError::UvNotFound);
        accepts_error(&UvError::CommandFailed("x".into()));
    }

    // ── env_dir_with_base ─────────────────────────────────────────────────────

    #[test]
    fn env_dir_uses_xdg_config_home_when_set() {
        // Use a temp dir to avoid relying on the real HOME value.
        let tmp = tempfile::tempdir().unwrap();
        let result = env_dir_with_base(Some(tmp.path()));
        assert_eq!(result, tmp.path().join("imhottep"));
    }

    #[test]
    fn env_dir_falls_back_to_home_dot_config() {
        // Temporarily clear XDG_CONFIG_HOME and set HOME to a known value.
        let tmp = tempfile::tempdir().unwrap();
        let orig_xdg = env::var_os("XDG_CONFIG_HOME");
        let orig_home = env::var_os("HOME");

        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
            env::set_var("HOME", tmp.path());
        }

        let result = env_dir_with_base(None);

        // Restore the environment unconditionally.
        unsafe {
            match orig_xdg {
                Some(v) => env::set_var("XDG_CONFIG_HOME", v),
                None => env::remove_var("XDG_CONFIG_HOME"),
            }
            match orig_home {
                Some(v) => env::set_var("HOME", v),
                None => env::remove_var("HOME"),
            }
        }

        assert_eq!(result, tmp.path().join(".config").join("imhottep"));
    }

    // ── UvNotFound propagation ────────────────────────────────────────────────
    //
    // These tests verify that every public function surfaces UvNotFound when
    // the uv binary cannot be found.  We achieve this by overriding PATH to an
    // empty directory so that no binary named "uv" can be resolved.

    fn with_no_uv<F: FnOnce() -> R, R>(f: F) -> R {
        let tmp = tempfile::tempdir().unwrap();
        let orig = env::var_os("PATH");
        unsafe {
            env::set_var("PATH", tmp.path());
        }
        let result = f();
        unsafe {
            match orig {
                Some(v) => env::set_var("PATH", v),
                None => env::remove_var("PATH"),
            }
        }
        result
    }

    fn base_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn add_dependency_returns_uv_not_found() {
        let tmp = base_dir();
        let err = with_no_uv(|| add_dependency_with_base("requests", Some(tmp.path())));
        assert!(
            matches!(err, Err(UvError::UvNotFound)),
            "expected UvNotFound, got: {err:?}"
        );
    }

    #[test]
    fn remove_dependency_returns_uv_not_found() {
        let tmp = base_dir();
        let err = with_no_uv(|| remove_dependency_with_base("requests", Some(tmp.path())));
        assert!(
            matches!(err, Err(UvError::UvNotFound)),
            "expected UvNotFound, got: {err:?}"
        );
    }

    #[test]
    fn list_dependencies_returns_uv_not_found() {
        let tmp = base_dir();
        let err = with_no_uv(|| list_dependencies_with_base(Some(tmp.path())));
        assert!(
            matches!(err, Err(UvError::UvNotFound)),
            "expected UvNotFound, got: {err:?}"
        );
    }

    #[test]
    fn reinit_environment_returns_uv_not_found() {
        let tmp = base_dir();
        let err = with_no_uv(|| reinit_environment_with_base(Some(tmp.path())));
        assert!(
            matches!(err, Err(UvError::UvNotFound)),
            "expected UvNotFound, got: {err:?}"
        );
    }

    #[test]
    fn run_code_returns_uv_not_found() {
        let tmp = base_dir();
        let err = with_no_uv(|| run_code_with_base("print(1)", Some(tmp.path())));
        assert!(
            matches!(err, Err(UvError::UvNotFound)),
            "expected UvNotFound, got: {err:?}"
        );
    }

    // ── reinit_environment removes existing .venv ─────────────────────────────

    #[test]
    fn reinit_removes_existing_venv_before_calling_uv() {
        // Create a fake .venv directory; reinit should delete it even before
        // calling uv (which will fail with UvNotFound here).
        let tmp = base_dir();
        let venv_path = tmp.path().join("imhottep").join(".venv");
        std::fs::create_dir_all(&venv_path).unwrap();
        assert!(venv_path.exists());

        with_no_uv(|| {
            let _ = reinit_environment_with_base(Some(tmp.path()));
        });

        assert!(
            !venv_path.exists(),
            ".venv should have been removed before uv was called"
        );
    }
}
