use std::{
    collections::{HashMap, HashSet},
    mem::size_of,
};
use windows::{
    core::{Result, PWSTR},
    Win32::{
        Foundation::{CloseHandle, ERROR_NO_MORE_FILES, FILETIME, HANDLE, STILL_ACTIVE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            RemoteDesktop::ProcessIdToSessionId,
            Threading::{
                GetExitCodeProcess, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
                PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    },
};

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub(super) fn current_session() -> Result<u32> {
    let mut session = 0;
    unsafe { ProcessIdToSessionId(std::process::id(), &mut session)? };
    Ok(session)
}

pub(super) fn in_session(pid: u32, expected: u32) -> bool {
    let mut session = 0;
    (unsafe { ProcessIdToSessionId(pid, &mut session) }).is_ok() && session == expected
}

pub(super) fn running_installer(path: &std::path::Path, session: u32) -> Result<bool> {
    let mut buffer = vec![0u16; 32768];
    let expected = path.to_string_lossy().to_lowercase();
    for row in process_snapshot()? {
        let end = row.szExeFile.iter().position(|ch| *ch == 0).unwrap_or(row.szExeFile.len());
        if String::from_utf16_lossy(&row.szExeFile[..end]).eq_ignore_ascii_case("SpotifySetup.exe")
            && in_session(row.th32ProcessID, session)
            && process_identity(row.th32ProcessID, &mut buffer).is_ok_and(|(path, _)| path.to_lowercase() == expected)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn process_snapshot() -> Result<Vec<PROCESSENTRY32W>> {
    let snapshot = Handle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)? });
    let mut row = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut rows = Vec::new();
    let mut result = unsafe { Process32FirstW(snapshot.0, &mut row) };
    loop {
        match result {
            Ok(()) => rows.push(row),
            Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => return Ok(rows),
            Err(error) => return Err(error),
        }
        result = unsafe { Process32NextW(snapshot.0, &mut row) };
    }
}

pub(super) fn process_tree(root_pid: u32) -> Result<Option<HashMap<u32, u32>>> {
    let rows = process_snapshot()?;
    if root_pid == 0 || !rows.iter().any(|row| row.th32ProcessID == root_pid) {
        return Ok(None);
    }
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for row in rows {
        children
            .entry(row.th32ParentProcessID)
            .or_default()
            .push(row.th32ProcessID);
    }
    let mut tree = HashMap::new();
    let mut pending = vec![(root_pid, root_pid)];
    while let Some((pid, parent)) = pending.pop() {
        if let std::collections::hash_map::Entry::Vacant(entry) = tree.entry(pid) {
            entry.insert(parent);
            if let Some(children) = children.get(&pid) {
                pending.extend(children.iter().map(|child| (*child, pid)));
            }
        }
    }
    Ok(Some(tree))
}

fn process_identity(pid: u32, buffer: &mut [u16]) -> Result<(String, f64)> {
    let handle = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)? });
    let mut size = buffer.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            handle.0,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )?;
    }
    Ok((
        String::from_utf16_lossy(&buffer[..size as usize]),
        creation_time(handle.0)?,
    ))
}

fn creation_time(handle: HANDLE) -> Result<f64> {
    let mut created = FILETIME::default();
    let mut others = [FILETIME::default(); 3];
    let [exited, kernel, user] = &mut others;
    unsafe { GetProcessTimes(handle, &mut created, exited, kernel, user)? };
    let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    let unix = ticks.saturating_sub(116_444_736_000_000_000);
    Ok(unix as f64 / 10_000_000.0)
}

pub(super) fn matches_creation(pid: u32, expected: f64) -> bool {
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return false;
    };
    let handle = Handle(handle);
    creation_time(handle.0).is_ok_and(|created| created == expected) && is_running(&handle)
}

fn is_running(handle: &Handle) -> bool {
    let mut code = 0;
    unsafe { GetExitCodeProcess(handle.0, &mut code) }.is_ok() && code == STILL_ACTIVE.0 as u32
}

fn lineage(tree: &HashMap<u32, u32>, root: u32, target: u32) -> Option<Vec<u32>> {
    let mut branch = vec![target];
    let mut current = target;
    let mut visited = HashSet::from([target]);
    while current != root {
        current = *tree.get(&current)?;
        if !visited.insert(current) {
            return None;
        }
        branch.push(current);
    }
    branch.reverse();
    Some(branch)
}

pub(super) fn verified_renderer(
    initial_tree: &HashMap<u32, u32>,
    root: u32,
    root_created: f64,
    target: u32,
) -> Result<Option<f64>> {
    let Some(branch) = lineage(initial_tree, root, target) else {
        return Ok(None);
    };
    let mut last_created = root_created;
    let mut handles = Vec::new();
    for (index, pid) in branch.iter().enumerate() {
        let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, *pid) })
        else {
            return Ok(None);
        };
        let handle = Handle(handle);
        let created = creation_time(handle.0)?;
        // A re-used ancestor PID may appear to be the parent of an older process.
        if (index == 0 && created != root_created) || created < last_created {
            return Ok(None);
        }
        last_created = created;
        handles.push(handle);
    }
    let same_branch = process_tree(root)?
        .and_then(|tree| lineage(&tree, root, target))
        .is_some_and(|current| current == branch);
    Ok((same_branch && handles.iter().all(is_running)).then_some(last_created))
}

pub(super) struct Process {
    pub pid: u32,
    pub parent: u32,
    pub created: f64,
    pub path: String,
}

pub(super) fn spotify_processes() -> Result<Vec<Process>> {
    let mut candidates = Vec::new();
    let mut buffer = vec![0u16; 32768];
    for row in process_snapshot()? {
        let end = row
            .szExeFile
            .iter()
            .position(|ch| *ch == 0)
            .unwrap_or(row.szExeFile.len());
        if !String::from_utf16_lossy(&row.szExeFile[..end]).eq_ignore_ascii_case("spotify.exe") {
            continue;
        }
        if let Ok((path, created)) = process_identity(row.th32ProcessID, &mut buffer) {
            candidates.push(Process {
                pid: row.th32ProcessID,
                parent: row.th32ParentProcessID,
                created,
                path,
            });
        }
    }
    Ok(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_renderer_lineage_through_all_intermediate_processes() {
        let tree = HashMap::from([(10, 10), (11, 10), (12, 11), (13, 12), (14, 12)]);
        assert_eq!(lineage(&tree, 10, 13), Some(vec![10, 11, 12, 13]));
        assert_eq!(lineage(&tree, 10, 14), Some(vec![10, 11, 12, 14]));
        assert_eq!(lineage(&tree, 10, 99), None);
        assert_eq!(lineage(&HashMap::from([(13, 12), (12, 13)]), 10, 13), None);
    }
}
