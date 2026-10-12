//! Listening TCP ports owned by a service.
//!
//! Read straight from the kernel's socket table rather than shelled out of
//! `netstat`, so it reports exactly what is bound and nothing that was merely
//! configured.
//!
//! The service's own PID is not enough. Neither database engine keeps its
//! sockets in the process the SCM reports: MySQL re-executes itself, so the
//! listening `mysqld.exe` is a *child* of the service process, and PostgreSQL's
//! service runs `pg_ctl.exe`, which spawns the `postgres.exe` that listens.
//! Ports are therefore collected across the service's whole process tree.

use std::io;
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::NetworkManagement::IpHelper::GetExtendedTcpTable;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

/// `AF_INET`. IPv4 only: enough to name the port a local database listens on,
/// and it keeps the table layout simple.
const AF_INET: u32 = 2;
/// `TCP_TABLE_OWNER_PID_LISTENER`: one row per listening socket, with its owner.
const TCP_TABLE_OWNER_PID_LISTENER: i32 = 3;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

/// Fields per `MIB_TCPROW_OWNER_PID`: state, local addr, local port, remote
/// addr, remote port, owning pid.
const ROW_WORDS: usize = 6;
const LOCAL_PORT: usize = 2;
const OWNING_PID: usize = 5;

/// Every TCP port any process in `root_pid`'s tree is listening on, ascending
/// and without duplicates.
pub fn listening_ports(root_pid: u32) -> io::Result<Vec<u16>> {
    let owners = process_tree(root_pid);
    let table = listener_table()?;

    let entries = table[0] as usize;
    let mut ports = Vec::new();
    for index in 0..entries {
        let row = 1 + index * ROW_WORDS;
        // A truncated table would be a bug in the API, not a real case.
        if row + ROW_WORDS > table.len() {
            break;
        }
        if owners.contains(&table[row + OWNING_PID]) {
            // The port is stored in network order in the low 16 bits.
            ports.push(u16::from_be(table[row + LOCAL_PORT] as u16));
        }
    }

    ports.sort_unstable();
    ports.dedup();
    Ok(ports)
}

/// `root` plus every process descended from it.
///
/// A failure to snapshot degrades to just the root, which is still the right
/// answer for an engine that listens in its service process.
fn process_tree(root: u32) -> Vec<u32> {
    let mut tree = vec![root];
    let Some(children) = process_parents() else {
        return tree;
    };

    // Breadth-first over the snapshot, which is unordered.
    let mut index = 0;
    while index < tree.len() {
        let current = tree[index];
        index += 1;
        for (pid, parent) in &children {
            if *parent == current && !tree.contains(pid) {
                tree.push(*pid);
            }
        }
    }

    tree
}

/// `(pid, parent pid)` for every process, from the toolhelp snapshot.
#[expect(
    unsafe_code,
    reason = "CreateToolhelp32Snapshot and its iteration have no safe wrapper"
)]
fn process_parents() -> Option<Vec<(u32, u32)>> {
    // Safety: no arguments beyond the flag, which the API defines.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return None;
    }

    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

    let mut processes = Vec::new();
    // Safety: `snapshot` is a live handle and `entry` is the right size.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        processes.push((entry.th32ProcessID, entry.th32ParentProcessID));
        // Safety: as above; the API fills `entry` for the next process.
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }

    // Safety: `snapshot` came from CreateToolhelp32Snapshot and is not reused.
    unsafe { CloseHandle(snapshot) };
    Some(processes)
}

/// The listener table as `u32`s, so the buffer is naturally aligned and the rows
/// can be indexed without casting a byte slice.
#[expect(
    unsafe_code,
    reason = "GetExtendedTcpTable has no safe wrapper; the table is a raw Win32 struct"
)]
fn listener_table() -> io::Result<Vec<u32>> {
    // 16 KiB start: comfortably above a typical table, so the common case is
    // one call and no retry.
    let mut table: Vec<u32> = vec![0; 4096];

    loop {
        let mut size = (table.len() * std::mem::size_of::<u32>()) as u32;

        // Safety: the buffer is `size` bytes long and `size` says so; the API
        // writes at most that much and reports back the size it needs.
        let status = unsafe {
            GetExtendedTcpTable(
                table.as_mut_ptr().cast(),
                &mut size,
                0,
                AF_INET,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };

        match status {
            0 => return Ok(table),
            ERROR_INSUFFICIENT_BUFFER => {
                // `size` now holds what is needed; round up to whole words.
                let words = size as usize / std::mem::size_of::<u32>() + 1;
                table.resize(words, 0);
            }
            code => return Err(io::Error::from_raw_os_error(code as i32)),
        }
    }
}

#[cfg(test)]
mod tests {
    /// Manual check against the live machine, since CI has no database servers:
    ///
    /// ```text
    /// $env:DATABASER_TEST_PID = "<service pid>"
    /// cargo test -- --ignored --nocapture
    /// ```
    ///
    /// Compare with `netstat -ano`, remembering the listening PID is usually a
    /// child of the service PID.
    #[test]
    #[ignore = "reads the live socket table and process list"]
    fn prints_ports_for_a_pid_from_the_environment() {
        let pid = std::env::var("DATABASER_TEST_PID")
            .expect("set DATABASER_TEST_PID")
            .parse()
            .expect("DATABASER_TEST_PID must be a number");
        println!("listening ports for {pid}: {:?}", super::listening_ports(pid));
    }
}
