//! Stack-allocated fixed-capacity array vector (`BoundedArray<T, N>`),
//! modeled on `oven-sh/bun`'s `src/bun_core/bounded_array.rs` (zero heap allocation).

use std::ops::{Deref, DerefMut};

use crate::error::{CoreError, CoreResult};

/// Fixed-capacity stack vector for `Copy` items — zero heap allocation, LTO-inlinable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundedArray<T: Copy, const N: usize> {
    buf: [T; N],
    len: usize,
}

impl<T: Copy + Default, const N: usize> Default for BoundedArray<T, N> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + Default, const N: usize> BoundedArray<T, N> {
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self {
            buf: [T::default(); N],
            len: 0,
        }
    }
}

impl<T: Copy, const N: usize> BoundedArray<T, N> {
    #[inline]
    #[must_use]
    pub const fn new_filled(fill: T) -> Self {
        Self {
            buf: [fill; N],
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
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }

    #[inline]
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.buf[..self.len]
    }

    #[inline]
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.buf[..self.len]
    }

    #[inline]
    pub fn try_push(&mut self, value: T) -> CoreResult<()> {
        if self.len >= N {
            return Err(CoreError::capacity_exceeded("bounded_array", N));
        }
        self.buf[self.len] = value;
        self.len += 1;
        Ok(())
    }

    #[inline]
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(self.buf[self.len])
    }

    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
    }
}

impl<T: Copy, const N: usize> Deref for BoundedArray<T, N> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T: Copy, const N: usize> DerefMut for BoundedArray<T, N> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_array_enforces_exact_capacity_without_heap() {
        let mut arr: BoundedArray<u32, 3> = BoundedArray::new_filled(0);
        assert!(arr.is_empty());
        arr.try_push(10).expect("push 1");
        arr.try_push(20).expect("push 2");
        arr.try_push(30).expect("push 3");
        assert_eq!(arr.as_slice(), &[10, 20, 30]);
        assert_eq!(
            arr.try_push(40),
            Err(CoreError::CapacityExceeded {
                component: "bounded_array",
                limit: 3
            })
        );
        assert_eq!(arr.pop(), Some(30));
        assert_eq!(arr.len(), 2);
    }
}
