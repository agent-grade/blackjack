use std::ffi::OsStr;
use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Why a program cannot be run, told apart the way a shell tells it: a name
/// that resolves to nothing exits 127, a file that exists but cannot be
/// executed exits 126. portable-pty folds both into a formatted string, which
/// is why the lookup happens here, before anything is spawned.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    NotFound,
    NotExecutable,
    IsADirectory,
}

impl Failure {
    pub fn exit_code(&self) -> u32 {
        match self {
            Self::NotFound => 127,
            Self::NotExecutable | Self::IsADirectory => 126,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotFound => "command not found",
            Self::NotExecutable => "permission denied",
            Self::IsADirectory => "is a directory",
        })
    }
}

pub fn check(program: &str) -> Result<(), Failure> {
    check_in(program, std::env::var_os("PATH").as_deref())
}

fn check_in(program: &str, path: Option<&OsStr>) -> Result<(), Failure> {
    if program.contains('/') {
        return classify(Path::new(program));
    }
    // A name that is not executable in one directory can still be executable
    // in a later one; only when no directory has a runnable file does the
    // first unrunnable one become the answer.
    let mut blocked = None;
    for dir in path.into_iter().flat_map(std::env::split_paths) {
        match classify(&dir.join(program)) {
            Ok(()) => return Ok(()),
            Err(Failure::NotFound) => {}
            Err(failure) => {
                blocked.get_or_insert(failure);
            }
        }
    }
    Err(blocked.unwrap_or(Failure::NotFound))
}

fn classify(candidate: &Path) -> Result<(), Failure> {
    let Ok(meta) = std::fs::metadata(candidate) else {
        return Err(Failure::NotFound);
    };
    if meta.is_dir() {
        Err(Failure::IsADirectory)
    } else if meta.permissions().mode() & 0o111 == 0 {
        Err(Failure::NotExecutable)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct Dir(PathBuf);

    impl Dir {
        fn fresh(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("blackjack-resolve-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, name: &str, mode: u32) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            path
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn on_path(dirs: &[&Dir]) -> std::ffi::OsString {
        std::env::join_paths(dirs.iter().map(|d| &d.0)).unwrap()
    }

    #[test]
    fn a_name_on_no_path_directory_is_not_found() {
        let dir = Dir::fresh("missing");
        assert_eq!(
            check_in("nothing-here", Some(&on_path(&[&dir]))),
            Err(Failure::NotFound)
        );
    }

    #[test]
    fn an_executable_on_the_path_runs() {
        let dir = Dir::fresh("runs");
        dir.file("tool", 0o755);
        assert_eq!(check_in("tool", Some(&on_path(&[&dir]))), Ok(()));
    }

    #[test]
    fn a_file_without_execute_permission_is_refused_not_missing() {
        let dir = Dir::fresh("plain");
        dir.file("tool", 0o644);
        assert_eq!(
            check_in("tool", Some(&on_path(&[&dir]))),
            Err(Failure::NotExecutable)
        );
    }

    #[test]
    fn a_later_directory_can_still_supply_the_executable() {
        let first = Dir::fresh("first");
        let second = Dir::fresh("second");
        first.file("tool", 0o644);
        second.file("tool", 0o755);
        assert_eq!(check_in("tool", Some(&on_path(&[&first, &second]))), Ok(()));
    }

    #[test]
    fn a_path_with_a_slash_is_checked_where_it_points() {
        let dir = Dir::fresh("direct");
        let plain = dir.file("tool", 0o644);
        assert_eq!(
            check_in(plain.to_str().unwrap(), Some(OsStr::new(""))),
            Err(Failure::NotExecutable)
        );
        assert_eq!(
            check_in(dir.0.to_str().unwrap(), Some(OsStr::new(""))),
            Err(Failure::IsADirectory)
        );
        assert_eq!(
            check_in(dir.0.join("absent").to_str().unwrap(), None),
            Err(Failure::NotFound)
        );
    }

    #[test]
    fn exit_codes_match_a_shells() {
        assert_eq!(Failure::NotFound.exit_code(), 127);
        assert_eq!(Failure::NotExecutable.exit_code(), 126);
        assert_eq!(Failure::IsADirectory.exit_code(), 126);
    }
}
