use serde_json::Value;
use std::slice;

const BEGIN: &[u8] = b"<!-- Lummix wrist add-on BEGIN -->";
const END: &[u8] = b"<!-- Lummix wrist add-on END -->";
const BODY: &[u8] = b"</body>";
const WRIST_LIMIT: usize = 4 * 1024 * 1024;
const REQUIRED_LAYOUT: [&[u8]; 3] = [
    b"uiContainer",
    b"./_Shared/js/uiComponents.js",
    b"./_Shared/js/toolbar.js",
];

#[repr(C)]
#[derive(Default)]
pub struct Layout {
    start: usize,
    end: usize,
    insert: usize,
    compatible: u32,
}

fn matches<'a>(data: &'a [u8], needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    data.windows(needle.len())
        .enumerate()
        .filter_map(move |(index, window)| (window == needle).then_some(index))
}

fn inspect_layout(wrist: &[u8]) -> Result<Layout, u32> {
    let mut layout = Layout::default();
    let mut starts = matches(wrist, BEGIN);
    let mut ends = matches(wrist, END);
    let clean;
    let stock = match (starts.next(), ends.next()) {
        (None, None) => wrist,
        (Some(start), Some(end)) if starts.next().is_none() && ends.next().is_none() => {
            if end < start {
                return Err(2);
            }
            layout.start = start;
            layout.end = end + END.len();
            clean = [&wrist[..layout.start], &wrist[layout.end..]].concat();
            &clean
        }
        _ => return Err(1),
    };

    let mut closing_tags = stock
        .windows(BODY.len())
        .enumerate()
        .filter_map(|(index, bytes)| bytes.eq_ignore_ascii_case(BODY).then_some(index));
    if let Some(insert) = closing_tags.next() {
        layout.insert = insert;
        layout.compatible = u32::from(
            closing_tags.next().is_none()
                && REQUIRED_LAYOUT
                    .iter()
                    .all(|marker| matches(stock, marker).next().is_some()),
        );
    }
    Ok(layout)
}

/// Inspect an exact Wrist.html byte snapshot. Python retains the ownership of all buffers.
#[no_mangle]
pub unsafe extern "C" fn lummix_xsoverlay_layout(
    data: *const u8,
    length: usize,
    result: *mut Layout,
) -> u32 {
    if data.is_null() || result.is_null() || length > WRIST_LIMIT {
        return 3;
    }
    match inspect_layout(slice::from_raw_parts(data, length)) {
        Ok(layout) => {
            result.write(layout);
            0
        }
        Err(error) => error,
    }
}

#[repr(u32)]
#[derive(Debug, PartialEq)]
enum Ownership {
    Uninstalled,
    Installed,
    Current,
    InvalidReceipt,
    EditedManifest,
    EditedIcon,
    EditedWrist,
    Unrecognized,
    InvalidRequest,
}

fn digest(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| {
        text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn ownership(facts: &Value) -> Ownership {
    let receipt = &facts["receipt"];
    let folder = facts["directory"].as_bool().unwrap_or(false);
    let same_files = facts["sameFiles"].as_bool().unwrap_or(false);
    let block = facts["block"].as_str();
    let same_block = block.is_some() && block == facts["expectedBlock"].as_str();
    if facts["hasReceipt"].as_bool() == Some(true) {
        let files = &receipt["files"];
        let proof_app = digest(&files["app.json"]);
        let proof_icon = digest(&files["icon.png"]);
        let proof_block = digest(&receipt["block"]);
        if receipt["owner"] != "Lummix"
            || receipt["format"].as_u64() != Some(1)
            || files.as_object().map(|files| files.len()) != Some(2)
            || proof_app.is_none()
            || proof_icon.is_none()
            || proof_block.is_none()
        {
            return Ownership::InvalidReceipt;
        }
        if facts["manifest"]
            .as_str()
            .is_some_and(|hash| Some(hash) != proof_app)
        {
            return Ownership::EditedManifest;
        }
        if facts["icon"]
            .as_str()
            .is_some_and(|hash| Some(hash) != proof_icon)
        {
            return Ownership::EditedIcon;
        }
        if block.is_some() && block != proof_block {
            return Ownership::EditedWrist;
        }
    } else if folder || (block.is_some() && !same_block) {
        return Ownership::Unrecognized;
    }
    if same_files && same_block {
        Ownership::Current
    } else if facts["hasReceipt"].as_bool() == Some(true) || block.is_some() {
        Ownership::Installed
    } else {
        Ownership::Uninstalled
    }
}

/// Validate receipt structure and determine who owns the exact files in a snapshot.
#[no_mangle]
pub unsafe extern "C" fn lummix_xsoverlay_ownership(data: *const u8, length: usize) -> u32 {
    if data.is_null() || length > WRIST_LIMIT {
        return Ownership::InvalidRequest as u32;
    }
    match serde_json::from_slice(slice::from_raw_parts(data, length)) {
        Ok(facts) => ownership(&facts) as u32,
        Err(_) => Ownership::InvalidRequest as u32,
    }
}

#[repr(u32)]
enum Disposition {
    Installable,
    Ready,
    ReadyReadOnly,
    Update,
    Unsupported,
    UnsupportedOwned,
    Blocked,
}

/// Convert compatibility, ownership and permission signals into install status.
#[no_mangle]
pub extern "C" fn lummix_xsoverlay_disposition(
    compatible: bool,
    writable: bool,
    ownership: u32,
) -> u32 {
    let state = if !compatible {
        if ownership == Ownership::Uninstalled as u32 {
            Disposition::Unsupported
        } else {
            Disposition::UnsupportedOwned
        }
    } else if ownership == Ownership::Current as u32 {
        if writable {
            Disposition::Ready
        } else {
            Disposition::ReadyReadOnly
        }
    } else if !writable {
        Disposition::Blocked
    } else if ownership == Ownership::Installed as u32 {
        Disposition::Update
    } else {
        Disposition::Installable
    };
    state as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn marker_offsets_preserve_other_bytes_and_ignore_closing_tags_in_the_owned_block() {
        let before = b"\xef\xbb\xbf<body id=uiContainer>./_Shared/js/uiComponents.js ./_Shared/js/toolbar.js";
        let after = b"<!-- vendor --></BODY>\r\n";
        let wrist = [before.as_slice(), BEGIN, b"</body>", END, after].concat();
        let layout = inspect_layout(&wrist).unwrap();
        assert_eq!(&wrist[..layout.start], before);
        assert_eq!(&wrist[layout.end..], after);
        assert_eq!(&after[layout.insert - before.len()..], b"</BODY>\r\n");
        assert_eq!(layout.compatible, 1);

        assert_eq!(inspect_layout(&[END, BEGIN].concat()).err(), Some(2));
        assert_eq!(inspect_layout(BEGIN).err(), Some(1));
    }

    #[test]
    fn receipt_requires_exact_owned_digests_and_can_restore_missing_files() {
        let hash = "a".repeat(64);
        let changed = "b".repeat(64);
        let mut facts = json!({
            "hasReceipt":true,
            "receipt":{"owner":"Lummix","format":1,"files":{"app.json":hash,"icon.png":hash},"block":hash},
            "manifest":hash, "icon":null, "block":hash, "expectedBlock":hash, "sameFiles":false
        });
        assert_eq!(ownership(&facts), Ownership::Installed);
        facts["icon"] = json!(changed);
        assert_eq!(ownership(&facts), Ownership::EditedIcon);
        facts["receipt"]["files"]["user.txt"] = json!(hash);
        assert_eq!(ownership(&facts), Ownership::InvalidReceipt);
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Prepare,
    Write,
    Restore,
    RemoveCreatedFolder,
    FinishRemove,
}

const SKIPPED: u32 = 0;
const APPLIED: u32 = 1;
const CONFLICT: u32 = 2;
const BLOCKED: u32 = 3;
const ROLLBACK_INCOMPLETE: u32 = 4;

fn transaction(install: bool, mut apply: impl FnMut(Phase, u32) -> u32) -> u32 {
    // Write the ownership receipt first during install. Remove it last so a
    // partially applied removal can still be recognized after a restart.
    let order = if install { [0, 1, 2, 3] } else { [3, 1, 2, 0] };
    let mut created = false;
    if install {
        match apply(Phase::Prepare, 0) {
            SKIPPED => {}
            APPLIED => created = true,
            CONFLICT => return CONFLICT,
            _ => return BLOCKED,
        }
    }
    let mut written = Vec::with_capacity(order.len());
    for slot in order {
        match apply(Phase::Write, slot) {
            SKIPPED => {}
            APPLIED => written.push(slot),
            failure => {
                let mut restored = true;
                for prior in written.into_iter().rev() {
                    restored &= apply(Phase::Restore, prior) == APPLIED;
                }
                if created {
                    restored &= apply(Phase::RemoveCreatedFolder, 0) == APPLIED;
                }
                return if !restored {
                    ROLLBACK_INCOMPLETE
                } else if failure == CONFLICT {
                    CONFLICT
                } else {
                    BLOCKED
                };
            }
        }
    }
    if !install {
        apply(Phase::FinishRemove, 0);
    }
    APPLIED
}

type FileOperation = extern "C" fn(phase: u32, slot: u32) -> u32;

/// Execute the ordered commit and rollback through the host's file adapters.
#[no_mangle]
pub extern "C" fn lummix_xsoverlay_transaction(
    install: bool,
    operation: Option<FileOperation>,
) -> u32 {
    let Some(operation) = operation else {
        return BLOCKED;
    };
    transaction(install, |phase, slot| operation(phase as u32, slot))
}

#[cfg(test)]
mod transaction_tests {
    use super::*;

    #[test]
    fn restores_every_write_in_reverse_order_after_a_conflict() {
        let mut operations = Vec::new();
        let result = transaction(true, |phase, slot| {
            operations.push((phase, slot));
            match (phase, slot) {
                (Phase::Write, 3) => CONFLICT,
                (Phase::Restore, 1) => CONFLICT,
                _ => APPLIED,
            }
        });
        assert_eq!(result, ROLLBACK_INCOMPLETE);
        assert_eq!(
            &operations[5..],
            &[
                (Phase::Restore, 2),
                (Phase::Restore, 1),
                (Phase::Restore, 0),
                (Phase::RemoveCreatedFolder, 0),
            ]
        );
    }
}
