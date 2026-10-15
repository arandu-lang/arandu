//! Legacy `i64`-specialized generational arena state-machine tests.
//!
//! Production backends (Cranelift JIT and C) use the type-erased Gold ABI in
//! [`crate::gen_runtime_gold`].

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    #[derive(Debug)]
    enum GenSlotState {
        Vacant,
        Occupied(i64),
        Retired,
    }

    #[derive(Debug)]
    struct GenSlot {
        state: GenSlotState,
        generation: u32,
    }

    #[derive(Debug, Default)]
    struct GenArenaI64 {
        slots: Vec<GenSlot>,
        free_list: Vec<u32>,
    }

    impl GenArenaI64 {
        const fn new() -> Self {
            Self {
                slots: Vec::new(),
                free_list: Vec::new(),
            }
        }

        fn insert(&mut self, value: i64) -> Option<i64> {
            if let Some(index) = self.free_list.pop() {
                let slot = self.slots.get_mut(usize::try_from(index).ok()?)?;
                let generation = slot.generation.checked_add(1)?;
                slot.generation = generation;
                slot.state = GenSlotState::Occupied(value);
                return pack_ref(index, generation);
            }

            let index = u32::try_from(self.slots.len()).ok()?;
            index.checked_add(1)?;
            self.slots.try_reserve(1).ok()?;
            self.slots.push(GenSlot {
                state: GenSlotState::Occupied(value),
                generation: 1,
            });
            pack_ref(index, 1)
        }

        fn get(&self, handle: i64) -> Option<i64> {
            let (index, expected_generation) = unpack_ref(handle)?;
            let slot = self.slots.get(usize::try_from(index).ok()?)?;
            if slot.generation != expected_generation {
                return None;
            }
            match slot.state {
                GenSlotState::Occupied(value) => Some(value),
                GenSlotState::Vacant | GenSlotState::Retired => None,
            }
        }

        fn remove(&mut self, handle: i64) -> Option<i64> {
            let (index, expected_generation) = unpack_ref(handle)?;
            let slot_index = usize::try_from(index).ok()?;
            let slot = self.slots.get(slot_index)?;
            if slot.generation != expected_generation
                || !matches!(slot.state, GenSlotState::Occupied(_))
            {
                return None;
            }

            let retire = slot.generation == u32::MAX;
            if !retire {
                self.free_list.try_reserve(1).ok()?;
            }
            let slot = &mut self.slots[slot_index];
            let next = if retire {
                GenSlotState::Retired
            } else {
                GenSlotState::Vacant
            };
            let GenSlotState::Occupied(value) = std::mem::replace(&mut slot.state, next) else {
                return None;
            };
            if !retire {
                self.free_list.push(index);
            }
            Some(value)
        }

        fn set(&mut self, handle: i64, value: i64) -> Option<i64> {
            let (index, expected_generation) = unpack_ref(handle)?;
            let slot = self.slots.get_mut(usize::try_from(index).ok()?)?;
            if slot.generation != expected_generation {
                return None;
            }
            let GenSlotState::Occupied(current) = &mut slot.state else {
                return None;
            };
            *current = value;
            Some(handle)
        }

        fn upsert(&mut self, handle: i64, value: i64) -> Option<i64> {
            if handle == 0 {
                self.insert(value)
            } else {
                self.set(handle, value)
            }
        }
    }

    fn pack_ref(index: u32, generation: u32) -> Option<i64> {
        let encoded_index = index.checked_add(1)?;
        let bits = (u64::from(encoded_index) << 32) | u64::from(generation);
        Some(i64::from_ne_bytes(bits.to_ne_bytes()))
    }

    fn unpack_ref(handle: i64) -> Option<(u32, u32)> {
        let bits = u64::from_ne_bytes(handle.to_ne_bytes());
        let encoded_index = u32::try_from(bits >> 32).ok()?;
        let generation = u32::try_from(bits & u64::from(u32::MAX)).ok()?;
        if encoded_index == 0 || generation == 0 {
            return None;
        }
        Some((encoded_index - 1, generation))
    }

    #[test]
    fn zero_is_invalid_and_first_handle_is_not_zero() {
        let mut arena = GenArenaI64::new();
        assert_eq!(arena.get(0), None);
        assert_eq!(arena.remove(0), None);
        assert_ne!(arena.insert(42), Some(0));
        let inserted = arena.upsert(0, 7).unwrap();
        assert_ne!(inserted, 0);
        assert_eq!(arena.upsert(inserted, 8), Some(inserted));
        assert_eq!(arena.get(inserted), Some(8));
    }

    #[test]
    fn insert_get_remove_cycle_recycles_without_aba() {
        let mut arena = GenArenaI64::new();
        let first = arena.insert(42).unwrap();
        assert_eq!(arena.get(first), Some(42));
        assert_eq!(arena.set(first, 43), Some(first));
        assert_eq!(arena.get(first), Some(43));
        assert_eq!(arena.remove(first), Some(43));
        let second = arena.insert(99).unwrap();
        assert_eq!(arena.get(second), Some(99));
        assert_eq!(arena.get(first), None);
        assert_eq!(unpack_ref(first).unwrap().0, unpack_ref(second).unwrap().0);
        assert_ne!(unpack_ref(first).unwrap().1, unpack_ref(second).unwrap().1);
    }

    #[test]
    fn exhausted_slot_retires_instead_of_wrapping() {
        let mut arena = GenArenaI64 {
            slots: vec![GenSlot {
                state: GenSlotState::Occupied(7),
                generation: u32::MAX,
            }],
            free_list: Vec::new(),
        };
        let exhausted = pack_ref(0, u32::MAX).unwrap();
        assert_eq!(arena.remove(exhausted), Some(7));
        let replacement = arena.insert(8).unwrap();
        assert_eq!(unpack_ref(replacement).unwrap().0, 1);
        assert_eq!(arena.get(exhausted), None);
    }
}
