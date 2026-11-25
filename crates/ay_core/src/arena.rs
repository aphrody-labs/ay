//! Generational typed index arena (`SlotId`) — replaces `Box<dyn>` / `Rc` with flat contiguous storage.

use crate::error::{CoreError, CoreResult};

/// Compact 64-bit generational handle (`index: u32`, `generation: u32`).
/// Proven zero-padding & zero-copy transmutable via `google/zerocopy` (`#![forbid(unsafe_code)]`).
#[repr(C)]
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    zerocopy::FromBytes,
    zerocopy::IntoBytes,
    zerocopy::Immutable,
    zerocopy::KnownLayout,
)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SlotId {
    index: u32,
    generation: u32,
}

impl SlotId {
    /// Zero-copy 8-byte representation backed by `google/zerocopy`.
    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        zerocopy::IntoBytes::as_bytes(self)
    }

    /// Sound decode from an exact 8-byte wire/mmap slice.
    #[inline]
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        <Self as zerocopy::FromBytes>::read_from_bytes(bytes).ok()
    }
    #[inline]
    #[must_use]
    pub const fn new(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }

    #[inline]
    #[must_use]
    pub const fn index(self) -> u32 {
        self.index
    }

    #[inline]
    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

enum Slot<T> {
    Occupied {
        generation: u32,
        value: T,
    },
    Vacant {
        generation: u32,
        next_free: Option<u32>,
    },
}

/// Contiguous, LTO-friendly generational arena with O(1) insert, get, and remove.
pub struct Arena<T> {
    slots: Vec<Slot<T>>,
    free_head: Option<u32>,
    len: usize,
}

impl<T> Default for Arena<T> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Arena<T> {
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free_head: None,
            len: 0,
        }
    }

    #[inline]
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
            free_head: None,
            len: 0,
        }
    }

    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn insert(&mut self, value: T) -> CoreResult<SlotId> {
        if let Some(idx) = self.free_head {
            let slot = &mut self.slots[idx as usize];
            let (generation, next_free) = match slot {
                Slot::Vacant {
                    generation,
                    next_free,
                } => (*generation, *next_free),
                Slot::Occupied { .. } => return Err(CoreError::arena_corrupt(idx)),
            };
            self.free_head = next_free;
            *slot = Slot::Occupied { generation, value };
            self.len += 1;
            return Ok(SlotId::new(idx, generation));
        }

        let Ok(idx) = u32::try_from(self.slots.len()) else {
            return Err(CoreError::capacity_exceeded("arena", u32::MAX as usize));
        };
        self.slots.push(Slot::Occupied {
            generation: 1,
            value,
        });
        self.len += 1;
        Ok(SlotId::new(idx, 1))
    }

    #[inline]
    #[must_use]
    pub fn get(&self, id: SlotId) -> Option<&T> {
        match self.slots.get(id.index as usize)? {
            Slot::Occupied { generation, value } if *generation == id.generation => Some(value),
            _ => None,
        }
    }

    #[inline]
    #[must_use]
    pub fn get_mut(&mut self, id: SlotId) -> Option<&mut T> {
        match self.slots.get_mut(id.index as usize)? {
            Slot::Occupied { generation, value } if *generation == id.generation => Some(value),
            _ => None,
        }
    }

    #[inline]
    pub fn remove(&mut self, id: SlotId) -> Option<T> {
        let slot = self.slots.get_mut(id.index as usize)?;
        match slot {
            Slot::Occupied { generation, .. } if *generation == id.generation => {
                let next_gen = generation.wrapping_add(1).max(1);
                let prev = std::mem::replace(
                    slot,
                    Slot::Vacant {
                        generation: next_gen,
                        next_free: self.free_head,
                    },
                );
                self.free_head = Some(id.index);
                self.len -= 1;
                match prev {
                    Slot::Occupied { value, .. } => Some(value),
                    Slot::Vacant { .. } => None,
                }
            }
            _ => None,
        }
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (SlotId, &T)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(idx, slot)| match slot {
                Slot::Occupied { generation, value } => {
                    Some((SlotId::new(idx as u32, *generation), value))
                }
                Slot::Vacant { .. } => None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arena_reuses_slots_and_rejects_stale_generations() {
        let mut arena = Arena::with_capacity(4);
        let a = arena.insert("alpha").expect("insert alpha");
        let b = arena.insert("beta").expect("insert beta");
        assert_eq!(arena.len(), 2);
        assert_eq!(arena.get(a), Some(&"alpha"));

        assert_eq!(arena.remove(a), Some("alpha"));
        assert_eq!(arena.get(a), None);

        let c = arena.insert("gamma").expect("reuse slot");
        assert_eq!(c.index(), a.index());
        assert_ne!(c.generation(), a.generation());
        assert_eq!(arena.get(a), None);
        assert_eq!(arena.get(c), Some(&"gamma"));
        assert_eq!(arena.get(b), Some(&"beta"));
    }
}
