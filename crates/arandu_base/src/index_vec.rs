//! Type-indexed `Vec` and the `IdIndex` trait.
//!
//! [`IndexVec<I, T>`] is a `Vec<T>` whose elements are addressed by a typed
//! index `I` rather than a plain `usize`. This prevents accidental indexing
//! into the wrong arena (e.g. using a `LocalId` to index into a temp table).
//!
//! Define a custom index type with the `newtype_index!` macro.

use std::marker::PhantomData;
use std::ops::{Index, IndexMut};

/// A typed index suitable for use with [`IndexVec`].
///
/// Implementors are cheap to copy and compare, and bijectively convert to/from
/// `usize`. Use `newtype_index!` to derive this for a newtype wrapper.
pub trait IdIndex: Copy + PartialEq + Eq {
    /// Converts the index to a raw `usize` offset.
    fn to_usize(self) -> usize;
    /// Constructs the index from a raw `usize` offset.
    fn from_usize(value: usize) -> Self;
}

impl IdIndex for usize {
    #[inline]
    fn to_usize(self) -> usize {
        self
    }
    #[inline]
    fn from_usize(value: usize) -> Self {
        value
    }
}

/// A `Vec<T>` indexed by a typed key `I` rather than a plain `usize`.
///
/// Prevents accidentally indexing one arena with an index from another. The
/// underlying storage is `pub raw: Vec<T>` for interop with code that needs
/// slice access without going through the typed API.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IndexVec<I: IdIndex, T> {
    pub raw: Vec<T>,
    _marker: PhantomData<I>,
}

impl<I: IdIndex, T> IndexVec<I, T> {
    /// Creates an empty `IndexVec`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            raw: Vec::new(),
            _marker: PhantomData,
        }
    }

    /// Creates an empty `IndexVec` with at least the given capacity pre-allocated.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            raw: Vec::with_capacity(capacity),
            _marker: PhantomData,
        }
    }

    /// Appends `val` and returns the typed index it was assigned.
    pub fn push(&mut self, val: T) -> I {
        let idx = self.raw.len();
        self.raw.push(val);
        I::from_usize(idx)
    }

    /// Extends the vec with `values` and returns the typed index range `[start, end)`.
    pub fn push_many<Iter>(&mut self, values: Iter) -> std::ops::Range<I>
    where
        Iter: IntoIterator<Item = T>,
    {
        let start = self.raw.len();
        self.raw.extend(values);
        I::from_usize(start)..I::from_usize(self.raw.len())
    }

    /// Returns a reference to the element at `index`, or `None` if out of bounds.
    pub fn get(&self, index: I) -> Option<&T> {
        self.raw.get(index.to_usize())
    }

    /// Returns a mutable reference to the element at `index`, or `None` if out of bounds.
    pub fn get_mut(&mut self, index: I) -> Option<&mut T> {
        self.raw.get_mut(index.to_usize())
    }

    /// Returns the number of elements.
    pub fn len(&self) -> usize {
        self.raw.len()
    }

    /// Returns `true` if there are no elements.
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Iterates over element references in insertion order.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.raw.iter()
    }

    /// Iterates over mutable element references in insertion order.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.raw.iter_mut()
    }

    /// Iterates over the typed indices of all elements.
    pub fn ids(&self) -> impl Iterator<Item = I> + '_ {
        (0..self.raw.len()).map(I::from_usize)
    }

    /// Returns a slice of all elements.
    pub fn as_slice(&self) -> &[T] {
        &self.raw
    }
}
impl<I: IdIndex, T> Index<I> for IndexVec<I, T> {
    type Output = T;
    fn index(&self, index: I) -> &Self::Output {
        &self.raw[index.to_usize()]
    }
}

impl<I: IdIndex, T> IndexMut<I> for IndexVec<I, T> {
    fn index_mut(&mut self, index: I) -> &mut Self::Output {
        &mut self.raw[index.to_usize()]
    }
}

impl<I: IdIndex, T> Default for IndexVec<I, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I: IdIndex, T> From<Vec<T>> for IndexVec<I, T> {
    fn from(raw: Vec<T>) -> Self {
        Self {
            raw,
            _marker: PhantomData,
        }
    }
}

/// Derives a `u32`-backed typed index implementing [`IdIndex`] for use with [`IndexVec`].
///
/// # Example
/// ```ignore
/// newtype_index!(FuncId);
/// let mut vec: IndexVec<FuncId, &str> = IndexVec::new();
/// let id: FuncId = vec.push("hello");
/// assert_eq!(vec[id], "hello");
/// ```
#[macro_export]
macro_rules! newtype_index {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u32);

        impl $name {
            #[must_use]
            #[inline]
            pub const fn as_usize(self) -> usize {
                self.0 as usize
            }

            #[must_use]
            #[inline]
            pub const fn from_usize(v: usize) -> Self {
                debug_assert!(v <= u32::MAX as usize, "index overflow for newtype_index");
                Self(v as u32)
            }
        }

        impl $crate::index_vec::IdIndex for $name {
            #[inline]
            fn to_usize(self) -> usize {
                self.0 as usize
            }

            #[inline]
            fn from_usize(value: usize) -> Self {
                Self::from_usize(value)
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    newtype_index!(TestId);

    #[test]
    fn ids_iterate_over_dense_indices() {
        let mut vec = IndexVec::<TestId, i32>::with_capacity(2);
        assert_eq!(TestId::from_usize(7).as_usize(), 7);
        assert_eq!(vec.push(10), TestId(0));
        assert_eq!(vec.push(20), TestId(1));
        assert_eq!(vec.ids().collect::<Vec<_>>(), vec![TestId(0), TestId(1)]);
        assert_eq!(vec.as_slice(), &[10, 20]);
    }

    #[test]
    fn push_many_returns_typed_range() {
        let mut vec = IndexVec::<TestId, i32>::new();
        vec.push(1);
        let range = vec.push_many([2, 3, 4]);
        assert_eq!(range, TestId(1)..TestId(4));
        assert_eq!(vec.as_slice(), &[1, 2, 3, 4]);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    #[cfg_attr(
        debug_assertions,
        should_panic(expected = "index overflow for newtype_index")
    )]
    fn from_usize_overflow_panics_in_debug() {
        if !cfg!(debug_assertions) {
            return;
        }
        let _ = TestId::from_usize((u32::MAX as usize) + 1);
    }
}
