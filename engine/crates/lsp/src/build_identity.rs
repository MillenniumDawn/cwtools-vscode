//! Build identity is embedded at compilation, never inferred from the current
//! workspace after the binary has been moved or installed.

fn valid_commit(value: Option<&str>) -> Option<&str> {
    value.filter(|sha| sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

pub(crate) fn build_commit() -> Option<&'static str> {
    valid_commit(option_env!("CWTOOLS_BUILD_COMMIT"))
}

fn render_version(version: &str, commit: Option<&str>) -> String {
    match valid_commit(commit) {
        Some(commit) => format!("cwtools-server {version} (source commit {commit})"),
        None => format!("cwtools-server {version} (source commit unavailable)"),
    }
}

/// Version and source identity of this compiled server binary.
pub fn server_version() -> String {
    render_version(env!("CARGO_PKG_VERSION"), build_commit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_distinguish_commits_and_handle_unavailable_metadata() {
        let first = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let second = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        assert_ne!(
            render_version("3.4.0", Some(first)),
            render_version("3.4.0", Some(second))
        );
        assert!(render_version("3.4.0", Some(first)).contains(first));
        for invalid in [None, Some("short"), Some("/home/user/private/source")] {
            assert_eq!(
                render_version("3.4.0", invalid),
                "cwtools-server 3.4.0 (source commit unavailable)"
            );
        }
    }
}
