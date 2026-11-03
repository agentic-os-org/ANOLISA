//! Count Brotli's ring buffers and Huffman allocations as retained stream bytes.

use brotli::{Allocator, HeapAlloc, SliceWrapper};
use std::cell::Cell;
use std::rc::Rc;

pub(super) struct CountingAllocator<T: Clone> {
    inner: HeapAlloc<T>,
    bytes: Rc<Cell<usize>>,
}

impl<T: Clone + Default> CountingAllocator<T> {
    pub(super) fn new(bytes: Rc<Cell<usize>>) -> Self {
        Self {
            inner: HeapAlloc::default(),
            bytes,
        }
    }
}

impl<T: Clone> Allocator<T> for CountingAllocator<T> {
    type AllocatedMemory = <HeapAlloc<T> as Allocator<T>>::AllocatedMemory;
    fn alloc_cell(&mut self, length: usize) -> Self::AllocatedMemory {
        let memory = self.inner.alloc_cell(length);
        self.bytes.set(
            self.bytes
                .get()
                .saturating_add(std::mem::size_of_val(memory.slice())),
        );
        memory
    }
    fn free_cell(&mut self, memory: Self::AllocatedMemory) {
        self.bytes.set(
            self.bytes
                .get()
                .saturating_sub(std::mem::size_of_val(memory.slice())),
        );
        self.inner.free_cell(memory);
    }
}
