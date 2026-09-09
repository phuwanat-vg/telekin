//! Keyboard-mapping arithmetic for the X11 injector, kept free of X types so
//! it is testable on any platform.
//!
//! An X keyboard mapping is a row of keysyms per keycode: index 0 is the
//! plain key, index 1 the same key with Shift, and further pairs belong to
//! other layout groups. Typing a character means finding a keycode whose row
//! produces it, and knowing whether Shift is part of the deal; characters the
//! layout lacks are typed by temporarily giving an unused keycode that
//! keysym.

use std::collections::HashMap;

/// What the server's keyboard mapping can produce, and what it leaves free.
pub struct Layout {
    /// keysym -> (keycode, Shift needed), preferring a plain key in the first
    /// group over a shifted one, and either over another group.
    by_sym: HashMap<u32, (u8, bool)>,
    /// Keycodes with no keysym at all.
    free: Vec<u8>,
}

impl Layout {
    /// `keysyms` is the flat table `GetKeyboardMapping` returns for keycodes
    /// `min..`, `per` entries each.
    pub fn from_mapping(min: u8, per: usize, keysyms: &[u32]) -> Self {
        let mut by_sym: HashMap<u32, (u8, bool, usize)> = HashMap::new();
        let mut free = Vec::new();
        if per == 0 {
            return Self { by_sym: HashMap::new(), free };
        }
        for (i, row) in keysyms.chunks(per).enumerate() {
            let Ok(kc) = u8::try_from(usize::from(min) + i) else { break };
            if row.iter().all(|s| *s == 0) {
                free.push(kc);
                continue;
            }
            // Only the first two entries are reachable with the keys we
            // press: plain, and Shift. Anything beyond needs a group switch
            // or AltGr, which nothing here holds, so it is not a way to type.
            for (level, sym) in row.iter().take(2).enumerate() {
                if *sym == 0 {
                    continue;
                }
                // Plain before shifted.
                let better = by_sym.get(sym).is_none_or(|(_, _, have)| level < *have);
                if better {
                    by_sym.insert(*sym, (kc, level % 2 == 1, level));
                }
            }
        }
        Self {
            by_sym: by_sym.into_iter().map(|(s, (kc, sh, _))| (s, (kc, sh))).collect(),
            free,
        }
    }

    /// The keycode that produces `sym`, and whether Shift must be held.
    pub fn lookup(&self, sym: u32) -> Option<(u8, bool)> {
        self.by_sym.get(&sym).copied()
    }

    pub fn free_keycodes(&self) -> &[u8] {
        &self.free
    }
}

/// Spare keycodes for characters the layout does not have.
///
/// One scratch keycode, remapped and reset around every press, loses
/// characters: a client that fetches the map after the reset sees no keysym
/// and types nothing, and on a slow board that is the usual order of events.
/// A pool used round-robin leaves each mapping in place until the keycode
/// comes round again, many characters later.
pub struct ScratchPool {
    /// (keycode, keysym currently mapped to it; 0 = nothing yet).
    slots: Vec<(u8, u32)>,
    next: usize,
}

impl ScratchPool {
    pub fn new(free: &[u8], size: usize) -> Self {
        Self {
            slots: free.iter().take(size).map(|kc| (*kc, 0)).collect(),
            next: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The keycode to press for `sym`, and whether it must be mapped to
    /// `sym` first (false when a slot already carries it).
    pub fn slot_for(&mut self, sym: u32) -> Option<(u8, bool)> {
        if self.slots.is_empty() {
            return None;
        }
        if let Some((kc, _)) = self.slots.iter().find(|(_, s)| *s == sym) {
            return Some((*kc, false));
        }
        let i = self.next;
        self.next = (self.next + 1) % self.slots.len();
        self.slots[i].1 = sym;
        Some((self.slots[i].0, true))
    }

    /// Every keycode that currently carries a keysym, for putting back.
    pub fn mapped(&self) -> impl Iterator<Item = u8> + '_ {
        self.slots.iter().filter(|(_, s)| *s != 0).map(|(kc, _)| *kc)
    }
}

/// Latin-1 and ASCII map to their code point; everything else uses the
/// Unicode keysym range defined by the X protocol.
pub fn char_keysym(ch: char) -> u32 {
    let c = ch as u32;
    if c < 0x100 {
        c
    } else {
        c | 0x0100_0000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XK_SHIFT_L: u32 = 0xffe1;

    /// A slice of a US layout: 8/*, ;/:, a/A, Shift, and two empty keycodes.
    fn us() -> Layout {
        let per = 4;
        let rows: [[u32; 4]; 6] = [
            [0x38, 0x2a, 0, 0],       // keycode 10: 8 *
            [0x3b, 0x3a, 0, 0],       // keycode 11: ; :
            [0, 0, 0, 0],             // keycode 12: free
            [0x61, 0x41, 0, 0],       // keycode 13: a A
            [XK_SHIFT_L, 0, 0, 0],    // keycode 14: Shift_L
            [0, 0, 0, 0],             // keycode 15: free
        ];
        let flat: Vec<u32> = rows.iter().flatten().copied().collect();
        Layout::from_mapping(10, per, &flat)
    }

    #[test]
    fn plain_and_shifted_characters_resolve_to_the_key_under_them() {
        let l = us();
        assert_eq!(l.lookup(char_keysym('8')), Some((10, false)));
        assert_eq!(l.lookup(char_keysym('*')), Some((10, true)), "* is Shift+8");
        assert_eq!(l.lookup(char_keysym(':')), Some((11, true)), ": is Shift+;");
        assert_eq!(l.lookup(char_keysym('a')), Some((13, false)));
        assert_eq!(l.lookup(char_keysym('A')), Some((13, true)));
        assert_eq!(l.lookup(XK_SHIFT_L), Some((14, false)));
        assert_eq!(l.lookup(char_keysym('ก')), None, "Thai is not in a US layout");
        assert_eq!(l.free_keycodes(), &[12, 15]);
    }

    #[test]
    fn a_plain_key_beats_a_shifted_one_and_other_groups_do_not_count() {
        // keycode 10 has X only with Shift; keycode 11 has it plain but in
        // the second group; keycode 12 has it plain in the first group.
        let per = 4;
        let flat = [
            [0x61, 0x58, 0, 0],
            [0x62, 0x42, 0x58, 0],
            [0x58, 0x78, 0, 0],
        ]
        .iter()
        .flatten()
        .copied()
        .collect::<Vec<u32>>();
        let l = Layout::from_mapping(10, per, &flat);
        assert_eq!(l.lookup(0x58), Some((12, false)));
        // Without keycode 12, Shift on keycode 10 is the only way we can
        // actually press: a second-group symbol needs a group switch.
        let l = Layout::from_mapping(10, per, &flat[..8]);
        assert_eq!(l.lookup(0x58), Some((10, true)));
    }

    #[test]
    fn scratch_slots_rotate_and_reuse_a_mapping_already_in_place() {
        let mut pool = ScratchPool::new(&[12, 15, 99], 2);
        let thai_a = char_keysym('ก');
        let thai_b = char_keysym('ข');
        let thai_c = char_keysym('ค');
        assert_eq!(pool.slot_for(thai_a), Some((12, true)), "first use maps");
        assert_eq!(pool.slot_for(thai_b), Some((15, true)));
        assert_eq!(pool.slot_for(thai_a), Some((12, false)), "still mapped, no remap");
        assert_eq!(pool.slot_for(thai_c), Some((12, true)), "round robin takes the oldest slot");
        assert_eq!(pool.mapped().collect::<Vec<_>>(), vec![12, 15]);
    }

    #[test]
    fn no_free_keycode_means_no_scratch() {
        let mut pool = ScratchPool::new(&[], 8);
        assert!(pool.is_empty());
        assert_eq!(pool.slot_for(char_keysym('ก')), None);
    }

    #[test]
    fn keysyms_follow_the_x_convention() {
        assert_eq!(char_keysym('-'), 0x2d);
        assert_eq!(char_keysym('é'), 0xe9);
        assert_eq!(char_keysym('ก'), 0x0100_0e01);
    }
}
