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

/// Resolves an agent's program with the `PATH` the child will see: the spawn context's, else
/// the adapter's, else this process's.
pub(crate) fn resolve_program(
    agent: &str,
    program: &str,
    adapter_env: &[(String, String)],
    ctx_env: &[(String, String)],
) -> crate::Result<PathBuf> {
    let path_override = ctx_env
        .iter()
        .rev()
        .chain(adapter_env.iter().rev())
        .find(|(k, _)| k == "PATH")
        .map(|(_, v)| std::ffi::OsString::from(v));
    let path_var = path_override.or_else(|| std::env::var_os("PATH"));
    find_program(program, path_var.as_deref()).ok_or_else(|| crate::Error::NotInstalled {
        agent: agent.to_owned(),
        program: program.to_owned(),
    })
}
