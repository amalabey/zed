//! Executable resolution and subprocess execution for a command-line host implementation.
//!
//! This file and `host_twg.rs` are the only two permitted to know that the host is reached by
//! running a program (FR-057). Nothing above them sees a process, an argument or an exit status.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use collections::HashMap;
use futures::FutureExt as _;
use futures::future::Shared;
use gpui::{App, AppContext as _, Task, WeakEntity};
use project::ProjectEnvironment;
use util::command::{Stdio, new_command};

use crate::host::HostError;

/// The name of the environment variable a reviewer can set to point the feature at a specific
/// binary. Read inside this crate rather than through Zed's settings schema, because FR-066 rules
/// out a settings entry for this phase.
pub const EXECUTABLE_OVERRIDE_VAR: &str = "ZED_PULL_REQUEST_TOOL";

/// A resolved executable, together with the environment it must be run in.
#[derive(Clone, Debug)]
pub struct ResolvedExecutable {
    pub path: PathBuf,
    pub environment: Arc<HashMap<String, String>>,
    pub working_directory: Arc<Path>,
}

#[derive(Clone, Debug)]
pub struct ProcessOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

impl ProcessOutput {
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// Runs one host command per invocation, resolving the executable through the project's own shell
/// environment.
///
/// FR-062a exists because a Zed launched from the dock or a desktop launcher inherits a minimal
/// `PATH` that typically excludes the directories these tools install into. Reporting a tool the
/// reviewer can plainly run in their terminal as "not installed" is the specific failure this
/// avoids, so the lookup uses the directory's shell environment rather than the process `PATH`.
pub struct HostProcess {
    program: &'static str,
    working_directory: Arc<Path>,
    /// Shared so the shell environment is loaded once per host rather than once per invocation;
    /// loading it spawns a shell, which is far more expensive than the calls that follow.
    ///
    /// There is deliberately no way to invalidate this. A reviewer who installs the tool and then
    /// retries gets a fresh resolution because the panel builds a fresh host, which keeps this type
    /// free of interior mutability it would otherwise need for one rare path.
    resolved: Shared<Task<Result<ResolvedExecutable, HostError>>>,
}

impl HostProcess {
    pub fn new(
        program: &'static str,
        working_directory: Arc<Path>,
        environment: WeakEntity<ProjectEnvironment>,
        cx: &mut App,
    ) -> Self {
        let resolved = resolve_executable(program, working_directory.clone(), environment, cx);
        Self {
            program,
            working_directory,
            resolved,
        }
    }

    pub fn program(&self) -> &'static str {
        self.program
    }

    pub fn working_directory(&self) -> &Arc<Path> {
        &self.working_directory
    }

    /// Run one invocation on the background executor, capturing stdout and stderr separately.
    ///
    /// Dropping the returned [`Task`] **kills the child process** rather than merely discarding its
    /// result, which is what FR-069 and constitution Principle II require: a dropped task that left
    /// the tool running would leave its host requests in flight.
    pub fn run(&self, args: Vec<String>, cx: &App) -> Task<Result<ProcessOutput, HostError>> {
        let resolved = self.resolved.clone();
        cx.background_spawn(async move {
            let executable = resolved.await?;
            run_resolved(executable, args).await
        })
    }
}

fn resolve_executable(
    program: &'static str,
    working_directory: Arc<Path>,
    environment: WeakEntity<ProjectEnvironment>,
    cx: &mut App,
) -> Shared<Task<Result<ResolvedExecutable, HostError>>> {
    let environment_task = environment
        .update(cx, |environment, cx| {
            environment.directory_environment(working_directory.clone(), cx)
        })
        .ok();

    cx.background_spawn(async move {
        let shell_environment = match environment_task {
            Some(task) => task.await.unwrap_or_default(),
            None => HashMap::default(),
        };

        let path_variable = shell_environment.get("PATH").cloned();
        let override_path = shell_environment
            .get(EXECUTABLE_OVERRIDE_VAR)
            .cloned()
            .or_else(|| std::env::var(EXECUTABLE_OVERRIDE_VAR).ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        let path = locate_executable(program, override_path.as_deref(), path_variable.as_deref())?;

        Ok(ResolvedExecutable {
            path,
            environment: Arc::new(shell_environment),
            working_directory,
        })
    })
    .shared()
}

/// Find the executable, preferring an explicit override and otherwise searching the project's own
/// `PATH`. Failing here is always [`HostError::PrerequisiteMissing`] and never an authentication
/// failure — FR-062b requires the two to stay distinct.
fn locate_executable(
    program: &str,
    override_path: Option<&str>,
    path_variable: Option<&str>,
) -> Result<PathBuf, HostError> {
    if let Some(override_path) = override_path {
        let candidate = PathBuf::from(override_path);
        if candidate.is_file() {
            return Ok(candidate);
        }
        return Err(HostError::PrerequisiteMissing {
            detail: format!(
                "{EXECUTABLE_OVERRIDE_VAR} is set to {override_path}, which isn't an executable file"
            ),
        });
    }

    // The suite must need no network and no host access (quickstart.md). Searching a real `PATH` in
    // a test build would let a developer's own installed tool be found and invoked for real, which
    // would make the tests depend on their machine and on the host being reachable. Making that
    // impossible in code is worth more than remembering not to do it.
    #[cfg(test)]
    {
        let ignored = match path_variable {
            Some(_) => "the project's PATH was ignored",
            None => "there was no PATH to search",
        };
        return Err(HostError::PrerequisiteMissing {
            detail: format!(
                "`{program}` is never searched for in a test build ({ignored}); set \
                 {EXECUTABLE_OVERRIDE_VAR} to exercise a real executable"
            ),
        });
    }

    #[cfg(not(test))]
    {
        let search_path = path_variable.filter(|value| !value.is_empty());
        let located = match search_path {
            Some(search_path) => which::which_in(program, Some(search_path), ".").ok(),
            None => which::which(program).ok(),
        };

        located.ok_or_else(|| HostError::PrerequisiteMissing {
            detail: match search_path {
                Some(_) => format!("`{program}` wasn't found on this project's PATH"),
                None => format!("`{program}` wasn't found on PATH"),
            },
        })
    }
}

async fn run_resolved(
    executable: ResolvedExecutable,
    args: Vec<String>,
) -> Result<ProcessOutput, HostError> {
    let child = spawn_child(&executable, &args)?;

    // `output` consumes the child, so if this future is dropped the child is dropped with it — and
    // it was spawned with kill-on-drop, which is what makes cancellation reach the real work.
    let output = child.output().await.map_err(|error| {
        // A process that vanished mid-flight is far more likely to have been killed by our own
        // cancellation than to be a genuine failure worth reporting to the reviewer.
        if error.kind() == std::io::ErrorKind::Interrupted {
            HostError::Cancelled
        } else {
            HostError::Unreachable {
                detail: error.to_string(),
            }
        }
    })?;

    Ok(ProcessOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code(),
    })
}

/// The one place kill-on-drop is configured, so cancellation semantics are not spread across call
/// sites.
pub(crate) fn spawn_child(
    executable: &ResolvedExecutable,
    args: &[String],
) -> Result<util::command::Child, HostError> {
    let mut command = new_command(&executable.path);
    command
        .args(args)
        .current_dir(executable.working_directory.as_ref())
        .envs(executable.environment.iter())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            HostError::PrerequisiteMissing {
                detail: format!("{} could not be run: {error}", executable.path.display()),
            }
        } else {
            HostError::Unreachable {
                detail: format!("{} could not be run: {error}", executable.path.display()),
            }
        }
    })
}

/// Convenience for callers that already know where the executable is — used by the fixture-backed
/// tests, which point the override at a script.
pub fn run_directly(
    executable: ResolvedExecutable,
    args: Vec<String>,
    cx: &App,
) -> Task<Result<ProcessOutput, HostError>> {
    cx.background_spawn(async move { run_resolved(executable, args).await })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn resolved_shell() -> ResolvedExecutable {
        ResolvedExecutable {
            path: PathBuf::from("/bin/sh"),
            environment: Arc::new(HashMap::default()),
            working_directory: Arc::from(Path::new("/")),
        }
    }

    fn process_is_alive(pid: u32) -> bool {
        // Signal 0 performs the permission and existence checks without delivering a signal, which
        // is the standard way to ask "is this pid still there".
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    /// FR-069 / constitution gate 2: cancellation has to reach the real work. A dropped `Task` that
    /// left the tool running would leave its host requests in flight, so this asserts the child
    /// process itself is gone — not merely that the result was discarded.
    #[gpui::test]
    async fn dropping_an_in_flight_invocation_kills_the_child(cx: &mut TestAppContext) {
        cx.executor().allow_parking();

        let (pid_tx, pid_rx) = futures::channel::oneshot::channel();
        let task = cx.background_executor.spawn(async move {
            let child = spawn_child(&resolved_shell(), &["-c".into(), "sleep 30".into()])
                .expect("spawning /bin/sh should succeed");
            let _ = pid_tx.send(child.id());
            child.output().await
        });

        let pid = pid_rx.await.expect("the child's pid should be reported");
        assert!(
            process_is_alive(pid),
            "the child should be running before the task is dropped"
        );

        drop(task);

        // The kill is issued synchronously on drop, but reaping is not instantaneous on every
        // platform, so allow a bounded number of executor turns rather than asserting immediately.
        let mut still_alive = true;
        for _ in 0..200 {
            if !process_is_alive(pid) {
                still_alive = false;
                break;
            }
            cx.background_executor
                .timer(std::time::Duration::from_millis(10))
                .await;
        }

        assert!(
            !still_alive,
            "dropping the task must kill the child process, not just discard its result"
        );
    }

    #[gpui::test]
    async fn a_completed_invocation_reports_both_streams_and_the_exit_code(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();

        let output = cx
            .update(|cx| {
                run_directly(
                    resolved_shell(),
                    vec!["-c".into(), "printf out; printf err 1>&2; exit 3".into()],
                    cx,
                )
            })
            .await
            .expect("the invocation itself should succeed");

        assert_eq!(output.stdout, "out");
        assert_eq!(output.stderr, "err");
        assert_eq!(output.exit_code, Some(3));
        assert!(!output.succeeded());
    }

    /// Resolution in a test build never searches a real `PATH`, so the suite cannot reach a host.
    #[test]
    fn a_test_build_never_finds_a_real_executable_on_the_path() {
        // `sh` exists on every machine this runs on, so if PATH were searched this would resolve.
        let error = locate_executable("sh", None, Some("/bin:/usr/bin"))
            .expect_err("a test build must not search PATH");
        assert!(matches!(error, HostError::PrerequisiteMissing { .. }));
    }

    #[test]
    fn an_unresolvable_executable_is_a_missing_prerequisite_never_an_auth_failure() {
        // FR-062b: these two must never be conflated, because the remedies are different.
        let error = locate_executable("definitely-not-a-real-program-xyz", None, Some(""))
            .expect_err("a nonexistent program must not resolve");
        assert!(matches!(error, HostError::PrerequisiteMissing { .. }));

        let error = locate_executable("anything", Some("/nonexistent/path/to/tool"), None)
            .expect_err("an override pointing at nothing must not resolve");
        match error {
            HostError::PrerequisiteMissing { detail } => {
                assert!(
                    detail.contains(EXECUTABLE_OVERRIDE_VAR),
                    "the message must name the override so the reviewer knows what to fix: {detail}"
                );
            }
            other => panic!("expected PrerequisiteMissing, got {other:?}"),
        }
    }

    #[test]
    fn an_override_pointing_at_a_real_file_wins_over_the_path() {
        let path = locate_executable("sh", Some("/bin/sh"), Some("/nowhere"))
            .expect("an override naming a real file should resolve");
        assert_eq!(path, PathBuf::from("/bin/sh"));
    }
}
