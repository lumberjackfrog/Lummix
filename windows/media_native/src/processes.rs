use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    mem::size_of,
};
use windows::{
    core::{Result, PWSTR},
    Win32::{
        Foundation::{CloseHandle, ERROR_NO_MORE_FILES, FILETIME, HANDLE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            Threading::{
                GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION,
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

pub(super) fn process_tree(root_pid: u32) -> Result<Option<HashSet<u32>>> {
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
    let mut tree = HashSet::new();
    let mut pending = vec![root_pid];
    while let Some(pid) = pending.pop() {
        if tree.insert(pid) {
            if let Some(children) = children.get(&pid) {
                pending.extend(children);
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
    creation_time(handle.0).is_ok_and(|created| created == expected)
}

pub(super) fn spotify_processes() -> Result<Value> {
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
            candidates.push(json!([
                row.th32ProcessID,
                row.th32ParentProcessID,
                created,
                path
            ]));
        }
    }
    Ok(json!(candidates))
}
