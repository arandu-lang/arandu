use crate::index_vec::IdIndex;
use std::marker::PhantomData;

/// A high-performance, dense bitset designed for compiler analyses (liveness, definite initialization, DCE, SSA).
/// Operates on 64-bit words for maximum CPU throughput and cache locality.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitSet<I: IdIndex = usize> {
    words: Vec<u64>,
    _marker: PhantomData<I>,
}

impl<I: IdIndex> BitSet<I> {
    /// Creates a new, empty `BitSet`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            words: Vec::new(),
            _marker: PhantomData,
        }
    }

    /// Creates a new `BitSet` pre-allocated to hold elements up to `capacity`.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let num_words = capacity.div_ceil(64);
        Self {
            words: vec![0; num_words],
            _marker: PhantomData,
        }
    }

    /// Creates a new `BitSet` from raw words.
    #[must_use]
    pub fn from_words(words: Vec<u64>) -> Self {
        Self {
            words,
            _marker: PhantomData,
        }
    }

    /// Creates a new `BitSet` with all bits from 0 to `capacity` set to 1.
    #[must_use]
    pub fn all_set(capacity: usize) -> Self {
        let num_words = capacity.div_ceil(64);
        let mut words = vec![u64::MAX; num_words];
        if capacity > 0 {
            let rem = capacity % 64;
            if rem != 0
                && let Some(last) = words.last_mut()
            {
                *last &= (1u64 << rem) - 1;
            }
        }
        Self {
            words,
            _marker: PhantomData,
        }
    }

    /// Inserts `id` into the bitset. Returns `true` if the element was not already present.
    pub fn insert(&mut self, id: I) -> bool {
        let bit = id.to_usize();
        let word_idx = bit / 64;
        let bit_idx = bit % 64;
        if word_idx >= self.words.len() {
            self.words.resize(word_idx + 1, 0);
        }
        let mask = 1u64 << bit_idx;
        let old = self.words[word_idx];
        self.words[word_idx] |= mask;
        (old & mask) == 0
    }

    /// Removes `id` from the bitset. Returns `true` if the element was present.
    pub fn remove(&mut self, id: I) -> bool {
        let bit = id.to_usize();
        let word_idx = bit / 64;
        let bit_idx = bit % 64;
        if word_idx >= self.words.len() {
            return false;
        }
        let mask = 1u64 << bit_idx;
        let old = self.words[word_idx];
        self.words[word_idx] &= !mask;
        (old & mask) != 0
    }

    /// Checks if `id` is present in the bitset.
    #[must_use]
    pub fn contains(&self, id: I) -> bool {
        let bit = id.to_usize();
        let word_idx = bit / 64;
        let bit_idx = bit % 64;
        if word_idx >= self.words.len() {
            return false;
        }
        (self.words[word_idx] & (1u64 << bit_idx)) != 0
    }

    /// Clears all elements from the bitset, keeping the allocated capacity.
    pub fn clear(&mut self) {
        self.words.fill(0);
    }

    /// Returns `true` if the bitset contains no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|&w| w == 0)
    }

    /// Returns the number of set bits (elements) in the bitset.
    #[must_use]
    pub fn len(&self) -> usize {
        self.words.iter().map(|&w| w.count_ones() as usize).sum()
    }

    /// Returns `true` if this bitset is a superset of `other`.
    #[must_use]
    pub fn is_superset_of(&self, other: &Self) -> bool {
        let min_len = usize::min(self.words.len(), other.words.len());
        for i in 0..min_len {
            if (self.words[i] & other.words[i]) != other.words[i] {
                return false;
            }
        }
        for i in min_len..other.words.len() {
            if other.words[i] != 0 {
                return false;
            }
        }
        true
    }

    /// Computes the union in-place (`self |= other`).
    /// Returns `true` if `self` was changed.
    pub fn union_with(&mut self, other: &Self) -> bool {
        let mut changed = false;
        if other.words.len() > self.words.len() {
            self.words.resize(other.words.len(), 0);
        }
        for (w_self, &w_other) in self.words.iter_mut().zip(other.words.iter()) {
            let old = *w_self;
            let new = old | w_other;
            if old != new {
                *w_self = new;
                changed = true;
            }
        }
        changed
    }

    /// Computes the intersection in-place (`self &= other`).
    /// Returns `true` if `self` was changed.
    pub fn intersect_with(&mut self, other: &Self) -> bool {
        let mut changed = false;
        let len_self = self.words.len();
        let len_other = other.words.len();
        let min_len = usize::min(len_self, len_other);
        for i in 0..min_len {
            let old = self.words[i];
            let new = old & other.words[i];
            if old != new {
                self.words[i] = new;
                changed = true;
            }
        }
        for i in min_len..len_self {
            if self.words[i] != 0 {
                self.words[i] = 0;
                changed = true;
            }
        }
        changed
    }

    /// Computes the difference in-place (`self &= !other`).
    /// Returns `true` if `self` was changed.
    pub fn difference_with(&mut self, other: &Self) -> bool {
        let mut changed = false;
        let len_self = self.words.len();
        let len_other = other.words.len();
        let min_len = usize::min(len_self, len_other);
        for i in 0..min_len {
            let old = self.words[i];
            let new = old & !other.words[i];
            if old != new {
                self.words[i] = new;
                changed = true;
            }
        }
        changed
    }

    /// Returns an iterator over all elements present in the bitset.
    pub fn iter(&self) -> BitSetIter<'_, I> {
        BitSetIter {
            set: self,
            word_idx: 0,
            bit_idx: 0,
        }
    }
}

pub struct BitSetIter<'a, I: IdIndex> {
    set: &'a BitSet<I>,
    word_idx: usize,
    bit_idx: usize,
}

impl<'a, I: IdIndex> Iterator for BitSetIter<'a, I> {
    type Item = I;

    fn next(&mut self) -> Option<Self::Item> {
        while self.word_idx < self.set.words.len() {
            let word = self.set.words[self.word_idx];
            if word == 0 {
                self.word_idx += 1;
                self.bit_idx = 0;
                continue;
            }
            let remaining_word = word >> self.bit_idx;
            if remaining_word == 0 {
                self.word_idx += 1;
                self.bit_idx = 0;
                continue;
            }
            let tz = remaining_word.trailing_zeros() as usize;
            self.bit_idx += tz;
            let val = self.word_idx * 64 + self.bit_idx;
            self.bit_idx += 1;
            if self.bit_idx >= 64 {
                self.word_idx += 1;
                self.bit_idx = 0;
            }
            return Some(I::from_usize(val));
        }
        None
    }
}

/// A high-performance, dense 2D bit matrix designed for multi-block dataflow analyses.
/// Stored flat in a single `Vec<u64>` for excellent cache locality.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitMatrix<R: IdIndex = usize, C: IdIndex = usize> {
    words: Vec<u64>,
    num_rows: usize,
    num_cols: usize,
    words_per_row: usize,
    _marker: PhantomData<(R, C)>,
}

impl<R: IdIndex, C: IdIndex> BitMatrix<R, C> {
    /// Creates a new `BitMatrix` with `rows` rows and `cols` columns, all initialized to 0.
    #[must_use]
    pub fn new(rows: usize, cols: usize) -> Self {
        let words_per_row = cols.div_ceil(64);
        let total_words = rows
            .checked_mul(words_per_row)
            .expect("BitMatrix capacity overflow: rows * words_per_row exceeds usize::MAX");
        Self {
            words: vec![0; total_words],
            num_rows: rows,
            num_cols: cols,
            words_per_row,
            _marker: PhantomData,
        }
    }

    /// Inserts `col` into `row`. Returns `true` if it was not already present.
    pub fn insert(&mut self, row: R, col: C) -> bool {
        let r = row.to_usize();
        let c = col.to_usize();
        assert!(r < self.num_rows);
        assert!(c < self.num_cols);
        let word_idx = r * self.words_per_row + (c / 64);
        let bit_idx = c % 64;
        let mask = 1u64 << bit_idx;
        let old = self.words[word_idx];
        self.words[word_idx] |= mask;
        (old & mask) == 0
    }

    /// Removes `col` from `row`. Returns `true` if it was present.
    pub fn remove(&mut self, row: R, col: C) -> bool {
        let r = row.to_usize();
        let c = col.to_usize();
        assert!(r < self.num_rows);
        assert!(c < self.num_cols);
        let word_idx = r * self.words_per_row + (c / 64);
        let bit_idx = c % 64;
        let mask = 1u64 << bit_idx;
        let old = self.words[word_idx];
        self.words[word_idx] &= !mask;
        (old & mask) != 0
    }

    /// Checks if `col` is present in `row`.
    #[must_use]
    pub fn contains(&self, row: R, col: C) -> bool {
        let r = row.to_usize();
        let c = col.to_usize();
        assert!(r < self.num_rows);
        assert!(c < self.num_cols);
        let word_idx = r * self.words_per_row + (c / 64);
        let bit_idx = c % 64;
        (self.words[word_idx] & (1u64 << bit_idx)) != 0
    }

    /// Clears the entire matrix.
    pub fn clear(&mut self) {
        self.words.fill(0);
    }

    /// Clears all columns of `row`.
    pub fn clear_row(&mut self, row: R) {
        let r = row.to_usize();
        assert!(r < self.num_rows);
        let start = r * self.words_per_row;
        let end = start + self.words_per_row;
        self.words[start..end].fill(0);
    }

    /// Sets all columns of `row` to 1.
    pub fn set_row(&mut self, row: R) {
        let r = row.to_usize();
        assert!(r < self.num_rows);
        let start = r * self.words_per_row;
        let end = start + self.words_per_row;
        self.words[start..end].fill(u64::MAX);
        // Mask the tail of the row
        if self.num_cols > 0 {
            let rem = self.num_cols % 64;
            if rem != 0 {
                self.words[end - 1] &= (1u64 << rem) - 1;
            }
        }
    }

    /// Returns a new `BitSet` copy of the given row.
    #[must_use]
    pub fn row_set(&self, row: R) -> BitSet<C> {
        let r = row.to_usize();
        assert!(r < self.num_rows);
        let start = r * self.words_per_row;
        let end = start + self.words_per_row;
        BitSet::from_words(self.words[start..end].to_vec())
    }

    /// Overwrites the given row with a `BitSet`.
    pub fn set_row_from_set(&mut self, row: R, set: &BitSet<C>) {
        let r = row.to_usize();
        assert!(r < self.num_rows);
        let start = r * self.words_per_row;
        let end = start + self.words_per_row;
        let mut words_src = &set.words[..];
        if words_src.len() > self.words_per_row {
            words_src = &words_src[..self.words_per_row];
        }
        self.words[start..start + words_src.len()].copy_from_slice(words_src);
        if words_src.len() < self.words_per_row {
            self.words[start + words_src.len()..end].fill(0);
        }
    }

    /// Unions row `src` into row `dest` (`dest |= src`).
    /// Returns `true` if `dest` was changed.
    pub fn union_rows(&mut self, src: R, dest: R) -> bool {
        let s = src.to_usize();
        let d = dest.to_usize();
        assert!(s < self.num_rows);
        assert!(d < self.num_rows);
        if s == d {
            return false;
        }
        let mut changed = false;
        let start_s = s * self.words_per_row;
        let start_d = d * self.words_per_row;
        for i in 0..self.words_per_row {
            let old = self.words[start_d + i];
            let new = old | self.words[start_s + i];
            if old != new {
                self.words[start_d + i] = new;
                changed = true;
            }
        }
        changed
    }

    /// Intersects row `src` into row `dest` (`dest &= src`).
    /// Returns `true` if `dest` was changed.
    pub fn intersect_rows(&mut self, src: R, dest: R) -> bool {
        let s = src.to_usize();
        let d = dest.to_usize();
        assert!(s < self.num_rows);
        assert!(d < self.num_rows);
        if s == d {
            return false;
        }
        let mut changed = false;
        let start_s = s * self.words_per_row;
        let start_d = d * self.words_per_row;
        for i in 0..self.words_per_row {
            let old = self.words[start_d + i];
            let new = old & self.words[start_s + i];
            if old != new {
                self.words[start_d + i] = new;
                changed = true;
            }
        }
        changed
    }

    /// Returns an iterator over all elements in the given row.
    pub fn iter_row(&self, row: R) -> BitMatrixRowIter<'_, C> {
        let r = row.to_usize();
        assert!(r < self.num_rows);
        let start = r * self.words_per_row;
        let end = start + self.words_per_row;
        BitMatrixRowIter {
            slice: &self.words[start..end],
            word_idx: 0,
            bit_idx: 0,
            _marker: PhantomData,
        }
    }
}

pub struct BitMatrixRowIter<'a, C: IdIndex> {
    slice: &'a [u64],
    word_idx: usize,
    bit_idx: usize,
    _marker: PhantomData<C>,
}

impl<'a, C: IdIndex> Iterator for BitMatrixRowIter<'a, C> {
    type Item = C;

    fn next(&mut self) -> Option<Self::Item> {
        while self.word_idx < self.slice.len() {
            let word = self.slice[self.word_idx];
            if word == 0 {
                self.word_idx += 1;
                self.bit_idx = 0;
                continue;
            }
            let remaining_word = word >> self.bit_idx;
            if remaining_word == 0 {
                self.word_idx += 1;
                self.bit_idx = 0;
                continue;
            }
            let tz = remaining_word.trailing_zeros() as usize;
            self.bit_idx += tz;
            let val = self.word_idx * 64 + self.bit_idx;
            self.bit_idx += 1;
            if self.bit_idx >= 64 {
                self.word_idx += 1;
                self.bit_idx = 0;
            }
            return Some(C::from_usize(val));
        }
        None
    }
}

impl<I: IdIndex> Default for BitSet<I> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_contains_remove() {
        let mut set = BitSet::<usize>::new();
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);

        assert!(set.insert(10));
        assert!(set.insert(100));
        assert!(!set.insert(10));

        assert_eq!(set.len(), 2);
        assert!(!set.is_empty());

        assert!(set.contains(10));
        assert!(set.contains(100));
        assert!(!set.contains(50));

        assert!(set.remove(10));
        assert!(!set.contains(10));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn test_union_intersection_difference() {
        let mut a = BitSet::<usize>::new();
        a.insert(1);
        a.insert(2);

        let mut b = BitSet::<usize>::new();
        b.insert(2);
        b.insert(3);

        let mut u = a.clone();
        assert!(u.union_with(&b));
        assert!(u.contains(1));
        assert!(u.contains(2));
        assert!(u.contains(3));

        let mut i = a.clone();
        assert!(i.intersect_with(&b));
        assert!(!i.contains(1));
        assert!(i.contains(2));
        assert!(!i.contains(3));

        let mut d = a.clone();
        assert!(d.difference_with(&b));
        assert!(d.contains(1));
        assert!(!d.contains(2));
    }

    #[test]
    fn test_iter() {
        let mut set = BitSet::<usize>::new();
        set.insert(5);
        set.insert(63);
        set.insert(64);
        set.insert(129);

        let items: Vec<usize> = set.iter().collect();
        assert_eq!(items, vec![5, 63, 64, 129]);
    }

    #[test]
    fn test_all_set() {
        let set = BitSet::<usize>::all_set(130);
        assert_eq!(set.len(), 130);
        assert!(set.contains(0));
        assert!(set.contains(64));
        assert!(set.contains(129));
        assert!(!set.contains(130));
    }

    #[test]
    fn test_bit_matrix() {
        let mut matrix = BitMatrix::<usize, usize>::new(3, 130);
        assert!(!matrix.contains(0, 5));
        assert!(matrix.insert(0, 5));
        assert!(!matrix.insert(0, 5));
        assert!(matrix.contains(0, 5));
        assert!(!matrix.contains(1, 5));

        matrix.insert(0, 64);
        matrix.insert(0, 129);

        let row0: Vec<usize> = matrix.iter_row(0).collect();
        assert_eq!(row0, vec![5, 64, 129]);

        let row0_set = matrix.row_set(0);
        assert_eq!(row0_set.len(), 3);
        assert!(row0_set.contains(5));

        let mut matrix2 = BitMatrix::<usize, usize>::new(3, 130);
        matrix2.insert(0, 5);
        matrix2.insert(0, 10);

        matrix.set_row_from_set(1, &matrix2.row_set(0));
        assert!(matrix.contains(1, 5));
        assert!(matrix.contains(1, 10));
        assert!(!matrix.contains(1, 64));

        assert!(matrix.union_rows(0, 1));
        assert!(matrix.contains(1, 5));
        assert!(matrix.contains(1, 10));
        assert!(matrix.contains(1, 64));
        assert!(matrix.contains(1, 129));

        assert!(matrix.intersect_rows(0, 1));
        assert!(!matrix.contains(1, 10));
        assert!(matrix.contains(1, 5));
        assert!(matrix.contains(1, 64));
        assert!(matrix.contains(1, 129));

        matrix.clear_row(1);
        assert!(!matrix.contains(1, 5));

        matrix.set_row(1);
        assert!(matrix.contains(1, 0));
        assert!(matrix.contains(1, 129));
        assert_eq!(matrix.num_cols, 130);
    }

    #[test]
    #[should_panic(expected = "BitMatrix capacity overflow")]
    fn test_bit_matrix_overflow() {
        let _ = BitMatrix::<usize, usize>::new(usize::MAX, usize::MAX);
    }
}
