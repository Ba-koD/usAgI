//! Locate CLI executables shipped inside desktop apps.
use std::fs;
use std::path::PathBuf;

fn executable(provider: &str) -> String {
    format!("{provider}{}", std::env::consts::EXE_SUFFIX)
}

/// Preserve profiles created by versions that already installed a private CLI.
pub(crate) fn previously_installed(provider: &str) -> Option<PathBuf> {
    let version = match provider {
        "claude" => "2.1.323",
        "codex" => "rust-v0.167.1",
        _ => return None,
    };
    Some(
        crate::config::config_dir()
            .join("runtimes")
            .join(provider)
            .join(version)
            .join(executable(provider)),
    )
}

/// Only look for the official CLI inside the apps' known binary directories.
pub(crate) fn desktop_programs(provider: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let home = crate::logs::home_dir();
    if cfg!(target_os = "macos") {
        let applications = PathBuf::from("/Applications");
        let user = home.as_ref().map(|p| p.join("Applications"));
        let bundles: &[&str] = if provider == "codex" {
            &["ChatGPT.app", "Codex.app"]
        } else {
            &["Claude.app"]
        };
        for parent in std::iter::once(applications).chain(user) {
            for bundle in bundles {
                let resources = parent.join(bundle).join("Contents/Resources");
                paths.push(resources.join(executable(provider)));
                paths.push(resources.join("bin").join(executable(provider)));
            }
        }
        if provider == "claude" {
            if let Some(home) = home {
                add_cache(
                    &mut paths,
                    home.join("Library/Application Support/Claude/claude-code"),
                    provider,
                );
            }
        }
    }
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            if provider == "codex" {
                add_cache(&mut paths, local.join("OpenAI/Codex/bin"), provider);
                paths.push(local.join("Programs/OpenAI/Codex/bin/codex.exe"));
            } else {
                let claude = local
                    .join("Packages/Claude_pzs8sxrjxfjjc/LocalCache/Roaming/Claude/claude-code");
                add_cache(&mut paths, claude, provider);
            }
        }
        if provider == "claude" {
            if let Some(roaming) = std::env::var_os("APPDATA") {
                add_cache(
                    &mut paths,
                    PathBuf::from(roaming).join("Claude/claude-code"),
                    provider,
                );
            }
        }
    }
    paths
}

fn add_cache(paths: &mut Vec<PathBuf>, root: PathBuf, provider: &str) {
    let Some(entries) = fs::read_dir(root).ok() else {
        return;
    };
    let mut versions: Vec<_> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.path())
        .collect();
    versions.sort_by_cached_key(|path| {
        let modified = path.metadata().and_then(|meta| meta.modified()).ok();
        std::cmp::Reverse(modified)
    });
    let name = executable(provider);
    for version in versions.into_iter().take(8) {
        paths.push(version.join(&name));
        if provider == "claude" {
            if let Ok(inner) = fs::read_dir(version) {
                paths.extend(inner.flatten().take(8).map(|p| p.path().join(&name)));
            }
        }
    }
}
