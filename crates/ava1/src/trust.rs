//! The trust slot in the payload ELF (SPEC.md §5.1).

pub const MAGIC: &[u8; 9] = b"AVA1TRUST";
pub const SLOT_LEN: usize = 64;
/// Slot states: never stamped, a key, a key and a launch token (SPEC.md §5.2).
pub const STATE_EMPTY: u8 = 0;
pub const STATE_KEY: u8 = 1;
pub const STATE_LAUNCH: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrustError {
    #[error("this ELF has no AVA1 trust slot")]
    NoSlot,
    #[error("this ELF has {0} AVA1 trust slots; expected exactly one")]
    Many(usize),
}

/// Offsets of every slot: magic, state 0..=2, zeros at 10..16, and zeros at 48..64
/// unless the state says a launch token is there.
fn slots(elf: &[u8]) -> Vec<usize> {
    if elf.len() < SLOT_LEN {
        return Vec::new();
    }
    (0..=elf.len() - SLOT_LEN)
        .filter(|&o| {
            &elf[o..o + 9] == MAGIC
                && elf[o + 9] <= STATE_LAUNCH
                && elf[o + 10..o + 16].iter().all(|&b| b == 0)
                && (elf[o + 9] == STATE_LAUNCH || elf[o + 48..o + 64].iter().all(|&b| b == 0))
        })
        .collect()
}

fn the_slot(elf: &[u8]) -> Result<usize, TrustError> {
    match slots(elf).as_slice() {
        [o] => Ok(*o),
        [] => Err(TrustError::NoSlot),
        many => Err(TrustError::Many(many.len())),
    }
}

/// Writes `key` into the ELF's single trust slot (state 1; any launch token is cleared).
pub fn stamp(elf: &mut [u8], key: &[u8; 32]) -> Result<(), TrustError> {
    let o = the_slot(elf)?;
    elf[o + 9] = STATE_KEY;
    elf[o + 16..o + 48].copy_from_slice(key);
    elf[o + 48..o + 64].fill(0);
    Ok(())
}

/// Writes `key` and a launch `token` into the single trust slot (state 2): the node
/// proves to the holder of `token` that it is the one that was launched (SPEC.md §5.2).
pub fn stamp_launch(elf: &mut [u8], key: &[u8; 32], token: &[u8; 16]) -> Result<(), TrustError> {
    let o = the_slot(elf)?;
    elf[o + 9] = STATE_LAUNCH;
    elf[o + 16..o + 48].copy_from_slice(key);
    elf[o + 48..o + 64].copy_from_slice(token);
    Ok(())
}

/// What a client that sends the node's program does before it sends (SPEC §5.1): stamp `key`
/// (the client's AVA1 public key) so the node trusts that client without pairing, and — when
/// `token` gives one — a fresh launch token, so the client trusts the node without pairing
/// too. `token` is called only once the ELF is known to be stampable,
/// so no token is minted for an ELF that will not carry it. `Err` is a reason to log,
/// never a reason not to send: an unstamped program still runs, and the node falls
/// back to its pairing window.
pub fn stamp_program(
    elf: &mut [u8],
    key: Option<&[u8; 32]>,
    token: impl FnOnce() -> Option<[u8; 16]>,
) -> Result<(), String> {
    let key = key.ok_or("AVA1 trust not stamped: this client has no AVA1 identity")?;
    the_slot(elf).map_err(|e| format!("AVA1 trust not stamped: {e}"))?;
    match token() {
        Some(t) => stamp_launch(elf, key, &t),
        None => stamp(elf, key),
    }
    .map_err(|e| format!("AVA1 trust not stamped: {e}"))
}

/// The stamped key, if the ELF has exactly one stamped slot.
pub fn read(elf: &[u8]) -> Option<[u8; 32]> {
    match slots(elf).as_slice() {
        [o] if elf[*o + 9] != STATE_EMPTY => elf[*o + 16..*o + 48].try_into().ok(),
        _ => None,
    }
}

/// The launch token, if the ELF has exactly one slot and it carries one.
pub fn read_token(elf: &[u8]) -> Option<[u8; 16]> {
    match slots(elf).as_slice() {
        [o] if elf[*o + 9] == STATE_LAUNCH => elf[*o + 48..*o + 64].try_into().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf_with(slots: usize) -> Vec<u8> {
        let mut b = vec![0x11u8; 4096];
        b[..4].copy_from_slice(b"\x7fELF");
        for i in 0..slots {
            let at = 1000 + i * 200;
            b[at..at + SLOT_LEN].fill(0);
            b[at..at + 9].copy_from_slice(MAGIC);
        }
        b
    }

    #[test]
    fn stamping_writes_the_key_once_and_can_be_redone() {
        let mut elf = elf_with(1);
        assert_eq!(read(&elf), None);
        stamp(&mut elf, &[7; 32]).unwrap();
        assert_eq!(read(&elf), Some([7; 32]));
        assert_eq!(elf[1009], 1);
        stamp(&mut elf, &[8; 32]).unwrap();
        assert_eq!(read(&elf), Some([8; 32]));
    }

    #[test]
    fn stamp_program_reports_why_it_did_not_stamp() {
        let mut elf = elf_with(1);
        assert!(stamp_program(&mut elf, None, || None)
            .unwrap_err()
            .contains("no AVA1 identity"));
        assert_eq!(read(&elf), None);
        stamp_program(&mut elf, Some(&[3; 32]), || None).unwrap();
        assert_eq!(read(&elf), Some([3; 32]));
        let mut old_build = elf_with(0);
        let before = old_build.clone();
        assert!(stamp_program(&mut old_build, Some(&[3; 32]), || None).is_err());
        assert_eq!(old_build, before, "an ELF without a slot is sent unchanged");
    }

    #[test]
    fn a_launch_stamp_carries_the_token_and_a_key_stamp_clears_it() {
        let mut elf = elf_with(1);
        stamp_launch(&mut elf, &[7; 32], &[0xa5; 16]).unwrap();
        assert_eq!(elf[1009], 2);
        assert_eq!(read(&elf), Some([7; 32]));
        assert_eq!(read_token(&elf), Some([0xa5; 16]));
        // A stamped ELF is still found, and can be stamped again either way.
        stamp_launch(&mut elf, &[8; 32], &[0x5a; 16]).unwrap();
        assert_eq!(
            (read(&elf), read_token(&elf)),
            (Some([8; 32]), Some([0x5a; 16]))
        );
        stamp(&mut elf, &[9; 32]).unwrap();
        assert_eq!((read(&elf), read_token(&elf)), (Some([9; 32]), None));
        assert!(
            elf[1048..1064].iter().all(|&b| b == 0),
            "key-only stamp zeroes the token"
        );
    }

    #[test]
    fn stamp_program_mints_a_token_only_for_a_slot_it_can_stamp() {
        let mut elf = elf_with(1);
        let mut minted = 0;
        stamp_program(&mut elf, Some(&[3; 32]), || {
            minted += 1;
            Some([4; 16])
        })
        .unwrap();
        assert_eq!(
            (read(&elf), read_token(&elf)),
            (Some([3; 32]), Some([4; 16]))
        );
        // No token to give: the key alone, as before.
        stamp_program(&mut elf, Some(&[3; 32]), || None).unwrap();
        assert_eq!((read(&elf), read_token(&elf)), (Some([3; 32]), None));
        // No slot, or no key: nothing is minted.
        let mut old_build = elf_with(0);
        assert!(stamp_program(&mut old_build, Some(&[3; 32]), || {
            minted += 1;
            Some([4; 16])
        })
        .is_err());
        assert!(stamp_program(&mut elf_with(1), None, || {
            minted += 1;
            Some([4; 16])
        })
        .is_err());
        assert_eq!(minted, 1);
    }

    #[test]
    fn no_slot_or_two_slots_is_refused() {
        assert_eq!(stamp(&mut elf_with(0), &[1; 32]), Err(TrustError::NoSlot));
        assert_eq!(stamp(&mut elf_with(2), &[1; 32]), Err(TrustError::Many(2)));
    }

    #[test]
    fn the_magic_without_the_zero_padding_is_not_a_slot() {
        let mut elf = elf_with(1);
        elf[1000 + 12] = 0x55; // reserved byte not zero: some other data that happens to start "AVA1TRUST"
        assert_eq!(stamp(&mut elf, &[1; 32]), Err(TrustError::NoSlot));
    }
}
