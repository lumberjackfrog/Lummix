use super::{audio_sessions, processes, registry, store_paths};
use std::{
    collections::HashSet,
    env,
    path::{Component, Path, PathBuf, Prefix},
};
use windows::core::Result;

const DESKTOP: u32 = 1;
const STORE: u32 = 2;
const EXECUTABLE: &str = "Spotify.exe";

fn normalize(path: &Path) -> Option<String> {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return None;
    };
    if !matches!(prefix.kind(), Prefix::Disk(_)) || components.next() != Some(Component::RootDir) {
        return None;
    }
    let parts = components
        .map(|part| match part {
            Component::Normal(part) => part.to_str().map(str::to_lowercase),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    if parts.last()?.as_str() != "spotify.exe" {
        return None;
    }
    Some(format!(
        "{}\\{}",
        prefix.as_os_str().to_str()?.to_ascii_lowercase(),
        parts.join("\\")
    ))
}

fn is_windows_apps(path: &Path) -> bool {
    path.iter().any(|part| {
        part.to_str()
            .is_some_and(|part| part.eq_ignore_ascii_case("WindowsApps"))
    })
}

fn ordinary_desktop_paths() -> impl Iterator<Item = PathBuf> {
    let directories = [
        "APPDATA",
        "LOCALAPPDATA",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
    ];
    directories
        .into_iter()
        .filter_map(env::var_os)
        .filter(|value| !value.is_empty())
        .map(|root| PathBuf::from(root).join("Spotify").join(EXECUTABLE))
        .chain(
            env::var_os("LOCALAPPDATA")
                .filter(|value| !value.is_empty())
                .map(|root| {
                    PathBuf::from(root)
                        .join("Programs\\Spotify")
                        .join(EXECUTABLE)
                }),
        )
}

fn expand_vars(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut index = 0;
    let mut output = String::new();
    while index < chars.len() {
        let first = chars[index];
        let remaining = &chars[index + 1..];
        if first == '\'' {
            let consumed = remaining
                .iter()
                .position(|ch| *ch == first)
                .map_or(remaining.len() + 1, |end| end + 2);
            output.extend(chars[index..index + consumed].iter());
            index += consumed;
            continue;
        }
        if matches!(first, '%' | '$') && remaining.first() == Some(&first) {
            output.push(first);
            index += 2;
            continue;
        }
        let variable = match first {
            '%' => remaining
                .iter()
                .position(|ch| *ch == '%')
                .map(|end| (1, 1 + end, 2 + end)),
            '$' if remaining.first() == Some(&'{') => remaining
                .iter()
                .position(|ch| *ch == '}')
                .map(|end| (2, 1 + end, 2 + end)),
            '$' => {
                let len = remaining
                    .iter()
                    .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(**ch, '_' | '-'))
                    .count();
                (len > 0).then_some((1, 1 + len, 1 + len))
            }
            _ => None,
        };
        if let Some((start, end, consumed)) = variable {
            let key: String = chars[index + start..index + end].iter().collect();
            if let Ok(value) = env::var(key) {
                output.push_str(&value);
            } else {
                output.extend(chars[index..index + consumed].iter());
            }
            index += consumed;
        } else {
            output.push(first);
            index += 1;
        }
    }
    output
}

fn strip_icon_index(value: &str) -> &str {
    if let Some((path, suffix)) = value.rsplit_once(',') {
        let suffix = suffix.trim();
        let suffix = suffix.strip_prefix('-').unwrap_or(suffix);
        if !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()) {
            return path;
        }
    }
    value
}

fn registered_desktop_paths() -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for (field, value) in registry::desktop_registrations()? {
        if value.is_empty() || value.contains('\0') {
            continue;
        }
        let value = if field == "DisplayIcon" {
            strip_icon_index(&value)
        } else {
            &value
        };
        let mut path = PathBuf::from(expand_vars(value.trim().trim_matches('"')));
        if field == "InstallLocation" {
            path.push(EXECUTABLE);
        }
        if !is_windows_apps(&path) && normalize(&path).is_some() {
            paths.push(path);
        }
    }
    Ok(paths)
}

pub(super) fn desktop_executable(preferred: Option<&Path>) -> Result<Option<PathBuf>> {
    let mut conventional: Vec<_> = ordinary_desktop_paths().collect();
    if let Some(preferred) = preferred.and_then(normalize) {
        if let Some(index) = conventional
            .iter()
            .position(|path| normalize(path).as_ref() == Some(&preferred))
        {
            let preferred = conventional.remove(index);
            conventional.insert(0, preferred);
        }
    }
    if let Some(path) = conventional
        .into_iter()
        .find(|path| normalize(path).is_some() && path.is_file())
    {
        return Ok(Some(path));
    }
    Ok(registered_desktop_paths()?
        .into_iter()
        .find(|path| path.is_file()))
}

fn store_shape(path: &Path) -> bool {
    let Some(directory) = path.parent() else {
        return false;
    };
    let Some(grandparent) = directory.parent() else {
        return false;
    };
    if !grandparent
        .file_name()
        .and_then(|part| part.to_str())
        .is_some_and(|part| part.eq_ignore_ascii_case("WindowsApps"))
    {
        return false;
    }
    let Some(package) = directory
        .file_name()
        .and_then(|part| part.to_str())
        .map(str::to_ascii_lowercase)
    else {
        return false;
    };
    let Some(version_and_arch) = package
        .strip_prefix("spotifyab.spotifymusic_")
        .and_then(|package| package.strip_suffix("__zpdnekdrzrea0"))
    else {
        return false;
    };
    let Some((version, arch)) = version_and_arch.split_once('_') else {
        return false;
    };
    matches!(arch, "x86" | "x64" | "arm64" | "neutral")
        && !version.is_empty()
        && version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
}

fn official_default_store_path(path: &Path) -> bool {
    let Some(root) = env::var_os("ProgramFiles").filter(|value| !value.is_empty()) else {
        return false;
    };
    let Some(package) = path.parent().and_then(|part| part.file_name()) else {
        return false;
    };
    let standard = PathBuf::from(root)
        .join("WindowsApps")
        .join(package)
        .join(EXECUTABLE);
    normalize(&standard) == normalize(path)
}

pub(super) fn verified_processes() -> Result<Vec<(processes::Process, u32)>> {
    let conventional: HashSet<_> = ordinary_desktop_paths()
        .filter_map(|path| normalize(&path))
        .collect();
    let mut registered = None;
    let mut store = None;
    let mut result = Vec::new();
    for process in processes::spotify_processes()? {
        let path = Path::new(&process.path);
        let Some(normalized) = normalize(path) else {
            continue;
        };
        let is_store = is_windows_apps(path);
        if !conventional.contains(&normalized) {
            if is_store {
                if !store_shape(path) {
                    continue;
                }
                if !official_default_store_path(path) {
                    if store.is_none() {
                        store = Some(
                            store_paths()?
                                .into_iter()
                                .flatten()
                                .filter_map(|root| normalize(&Path::new(&root).join(EXECUTABLE)))
                                .collect::<HashSet<_>>(),
                        );
                    }
                    if !store.as_ref().unwrap().contains(&normalized) {
                        continue;
                    }
                }
            } else {
                if registered.is_none() {
                    registered = Some(
                        registered_desktop_paths()?
                            .into_iter()
                            .filter_map(|path| normalize(&path))
                            .collect::<HashSet<_>>(),
                    );
                }
                if !registered.as_ref().unwrap().contains(&normalized) {
                    continue;
                }
            }
        }
        result.push((process, if is_store { STORE } else { DESKTOP }));
    }
    Ok(result)
}

pub(super) type Source = (u32, f64, u32);

fn root() -> Result<(i32, Option<(processes::Process, u32)>)> {
    let processes = verified_processes()?;
    let ids: HashSet<_> = processes.iter().map(|(process, _)| process.pid).collect();
    let mut roots = processes
        .into_iter()
        .filter(|(process, _)| !ids.contains(&process.parent));
    let Some((process, edition)) = roots.next() else {
        return Ok((0, None));
    };
    if roots.next().is_some() {
        return Ok((2, None));
    }
    Ok((1, Some((process, edition))))
}

pub(super) fn diagnostic_target() -> Result<serde_json::Value> {
    let (state, root) = root()?;
    let Some((p, edition)) = root else {
        return Ok(serde_json::json!({"state": state}));
    };
    let Some(tree) = processes::process_tree(p.pid)? else {
        return Ok(serde_json::json!({"state": 4}));
    };
    let renderer = capture_renderer(&tree, p.pid, p.created)?.unwrap_or((p.pid, p.created));
    if !processes::matches_creation(p.pid, p.created)
        || !processes::matches_creation(renderer.0, renderer.1)
    {
        return Ok(serde_json::json!({"state": 4}));
    }
    Ok(
        serde_json::json!({"state": 1, "pid": renderer.0, "created": renderer.1,
                          "edition": if edition == DESKTOP { "desktop" } else { "store" }}),
    )
}

pub(super) fn probe(signed: &[String]) -> Result<(i32, Option<Source>)> {
    let (state, root) = root()?;
    let Some((process, edition)) = root else {
        return Ok((state, None));
    };
    let Some(tree) = processes::process_tree(process.pid)? else {
        return Ok((4, None));
    };
    let route = audio_sessions::direct_to_cable_in_tree(&tree, signed)?;
    let renderer = if route == Some(false) {
        capture_renderer(&tree, process.pid, process.created).unwrap_or(None)
    } else {
        None
    };
    if !processes::matches_creation(process.pid, process.created) {
        return Ok((4, None));
    }
    let source = (process.pid, process.created, edition);
    Ok(match route {
        Some(false) => {
            let target = renderer
                .filter(|(pid, created)| processes::matches_creation(*pid, *created))
                .map_or(source, |(pid, created)| (pid, created, edition));
            (1, Some(target))
        }
        Some(true) => (3, Some(source)),
        None => (4, None),
    })
}

fn capture_renderer(
    tree: &std::collections::HashMap<u32, u32>,
    root: u32,
    root_created: f64,
) -> Result<Option<(u32, f64)>> {
    let Some(pid) = audio_sessions::active_renderer(tree)? else {
        return Ok(None);
    };
    Ok(processes::verified_renderer(tree, root, root_created, pid)?.map(|created| (pid, created)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_remote_relative_and_ambiguous_executable_paths() {
        assert!(normalize(Path::new(r"C:\Users\demo\Spotify\Spotify.exe")).is_some());
        assert!(normalize(Path::new(r"C:\Users\demo\..\Spotify.exe")).is_none());
        assert!(normalize(Path::new(r"\\remote\music\Spotify.exe")).is_none());
        assert!(normalize(Path::new(r"C:Spotify.exe")).is_none());
        assert!(normalize(Path::new(r"C:\Music\Spotify.exe.bak")).is_none());
    }

    #[test]
    fn requires_exact_spotify_store_package_shape() {
        assert!(store_shape(Path::new(
            r"C:\WindowsApps\SpotifyAB.SpotifyMusic_1.0.0.0_x64__zpdnekdrzrea0\Spotify.exe"
        )));
        assert!(!store_shape(Path::new(
            r"C:\WindowsApps\SpotifyAB.SpotifyMusic_1.0.0.0_x64__other\Spotify.exe"
        )));
        assert!(!store_shape(Path::new(
            r"C:\Apps\SpotifyAB.SpotifyMusic_1.0.0.0_x64__zpdnekdrzrea0\Spotify.exe"
        )));
    }

    #[test]
    #[ignore = "requires a dedicated nonphysical render fixture"]
    fn selects_renderer_in_live_fixture_tree() {
        let fixture = env::var("LUMMIX_RENDERER_FIXTURE").unwrap();
        let (root, root_created, expected): (u32, f64, u32) =
            serde_json::from_str(&fixture).unwrap();
        let _apartment = super::super::Apartment::enter().unwrap();
        let tree = processes::process_tree(root).unwrap().unwrap();
        let (renderer, created) = capture_renderer(&tree, root, root_created)
            .unwrap()
            .unwrap();
        assert_ne!(renderer, root);
        assert_eq!(renderer, expected);
        assert!(processes::matches_creation(renderer, created));
    }
}
