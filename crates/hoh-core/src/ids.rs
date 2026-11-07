//! Derived protocol ids (at most 160 characters, the contract's bound), shared by the store and pure builders.

/// Protocol ids are at most 160 characters.
pub const MAX_ID: usize = 160;

/// Longest suffix kept as is in a derived id; longer ones are shortened like a long parent.
const MAX_SUFFIX: usize = 64;

/// `text` if it fits in `max` bytes (`max` > 17), else its first bytes tagged with its FNV-1a hash in exactly `max`.
fn shorten(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    // Room for "~" and the 16 hex digits.
    let mut cut = max.saturating_sub(17);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}~{hash:016x}", &text[..cut])
}

/// An id derived from `parent` and `suffix` that stays within [`MAX_ID`] for any input: a long suffix (callers pass
/// event names) or a long parent is shortened and tagged with its FNV-1a hash; were two ever to collide, the UNIQUE
/// constraint would fail the transaction rather than merge rows.
pub fn child_id(parent: &str, suffix: &str) -> String {
    let id = format!("{parent}/{suffix}");
    if id.len() <= MAX_ID {
        return id;
    }
    let suffix = shorten(suffix, MAX_SUFFIX);
    format!("{}/{suffix}", shorten(parent, MAX_ID - 1 - suffix.len()))
}
