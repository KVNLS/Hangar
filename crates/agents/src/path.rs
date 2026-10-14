use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Resolves `program` like a shell would: a name containing `/` is checked as given, a bare
/// name is searched in `path_var` (a `PATH`-style list). Empty `PATH` entries, which mean the
/// current directory, are skipped.
pub fn find_program(program: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    if program.contains('/') {
        let path = PathBuf::from(program);
        return is_executable(&path).then_some(path);
    }
    std::env::split_paths(path_var?)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}
