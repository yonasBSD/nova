// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    ecmascript::types::{DataBlock, OrdinaryObject},
    engine::context::{Bindable, NoGcScope},
    heap::{CompactionLists, HeapMarkAndSweep, WorkQueues},
};

// TODO: Investigate if the common case is that the byte length is less than
// an u16, that would mean we could squeeze an extra 2 bytes out of the struct.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct ViewedArrayBufferByteLength(pub u32);

impl ViewedArrayBufferByteLength {
    /// Returns `true` if the byte length is stored in an associated map in the
    /// heap.
    pub(crate) const fn is_overflowing(self) -> bool {
        self.0 == Self::heap().0
    }

    pub const fn value(value: u32) -> Self {
        Self(value)
    }

    /// A sentinel value of `u32::MAX - 1` means that the byte length is stored in an
    /// associated map in the heap. This will most likely be a very rare case,
    /// only applicable for 4GB+ buffers.
    pub const fn heap() -> Self {
        Self(u32::MAX - 1)
    }

    /// A sentinel value of `u32::MAX` means that the byte length is the
    /// `AUTO` value used in the spec.
    pub const fn auto() -> Self {
        Self(u32::MAX)
    }
}

impl Default for ViewedArrayBufferByteLength {
    fn default() -> Self {
        Self::auto()
    }
}

impl From<Option<usize>> for ViewedArrayBufferByteLength {
    fn from(value: Option<usize>) -> Self {
        match value {
            Some(value) => Self::from(value),
            None => Self::auto(),
        }
    }
}

impl From<usize> for ViewedArrayBufferByteLength {
    fn from(value: usize) -> Self {
        if value >= Self::heap().0 as usize {
            Self::heap()
        } else {
            Self::value(value as u32)
        }
    }
}

// TODO: Investigate if the common case is that the byte offset is less than
// an u16, that would mean we could squeeze an extra 2 bytes out of the struct.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct ViewedArrayBufferByteOffset(pub u32);

impl ViewedArrayBufferByteOffset {
    pub(crate) const fn value(value: u32) -> Self {
        Self(value)
    }

    pub(crate) fn is_overflowing(self) -> bool {
        self.0 == Self::heap().0
    }

    /// A sentinel value of `u32::MAX` means that the byte offset is stored in
    /// an associated map in the heap. This will most likely be a very rare
    /// case, only applicable for 4GB+ buffers.
    pub(crate) const fn heap() -> Self {
        Self(u32::MAX)
    }
}

impl Default for ViewedArrayBufferByteOffset {
    fn default() -> Self {
        Self::value(0)
    }
}

impl From<usize> for ViewedArrayBufferByteOffset {
    fn from(value: usize) -> Self {
        if value >= Self::heap().0 as usize {
            Self::heap()
        } else {
            Self::value(value as u32)
        }
    }
}

#[derive(Debug)]
pub(crate) struct InternalBuffer {
    data_block: DataBlock,
    /// Capacity of a resizable ArrayBuffer, or usize::MAX if the buffer is not
    /// resizable.
    capacity: usize,
}

impl InternalBuffer {
    /// Returns the contained DataBlock.
    ///
    /// Panics if the buffer is detached.
    pub(crate) fn get_data_block(&self) -> &DataBlock {
        if self.data_block.is_detached() {
            unreachable!();
        }
        &self.data_block
    }

    /// Returns the contained DataBlock.
    ///
    /// Panics if the buffer is detached.
    pub(crate) fn get_data_block_mut(&mut self) -> &mut DataBlock {
        if self.data_block.is_detached() {
            unreachable!();
        }
        &mut self.data_block
    }

    pub(crate) fn detach(&mut self) {
        self.capacity = 0;
        self.data_block = DataBlock::DETACHED_DATA_BLOCK;
    }

    const fn detached() -> Self {
        Self {
            data_block: DataBlock::DETACHED_DATA_BLOCK,
            capacity: usize::MAX,
        }
    }

    fn fixed_length(data_block: DataBlock) -> Self {
        Self {
            data_block,
            capacity: usize::MAX,
        }
    }

    fn resizable(data_block: DataBlock, capacity: usize) -> Self {
        assert_ne!(capacity, usize::MAX);
        Self {
            data_block,
            capacity,
        }
    }
}

#[derive(Debug)]
pub struct ArrayBufferHeapData<'a> {
    pub(crate) object_index: Option<OrdinaryObject<'a>>,
    pub(super) buffer: InternalBuffer,
    // detach_key
}

impl Default for ArrayBufferHeapData<'_> {
    #[inline(always)]
    fn default() -> Self {
        Self {
            object_index: None,
            buffer: InternalBuffer::detached(),
        }
    }
}

unsafe impl Send for ArrayBufferHeapData<'_> {}

impl ArrayBufferHeapData<'_> {
    pub(crate) fn new_resizable(db: DataBlock, cap: usize) -> Self {
        Self {
            object_index: None,
            buffer: InternalBuffer::resizable(db, cap),
        }
    }

    pub(crate) fn new_fixed_length(db: DataBlock) -> Self {
        Self {
            object_index: None,
            buffer: InternalBuffer::fixed_length(db),
        }
    }

    /// Returns the contained DataBlock.
    ///
    /// Panics if the buffer is detached.
    pub(crate) fn get_data_block(&self) -> &DataBlock {
        &self.buffer.data_block
    }

    /// Returns the contained DataBlock.
    ///
    /// Panics if the buffer is detached.
    pub(crate) fn get_data_block_mut(&mut self) -> &mut DataBlock {
        &mut self.buffer.data_block
    }

    pub(crate) fn is_detached(&self) -> bool {
        self.buffer.data_block.is_detached()
    }

    pub(crate) fn is_resizable(&self) -> bool {
        self.buffer.capacity != usize::MAX
    }

    pub(crate) fn byte_length(&self) -> usize {
        if self.is_detached() {
            0
        } else {
            self.buffer.data_block.len()
        }
    }

    pub(crate) fn max_byte_length(&self) -> usize {
        if self.is_detached() {
            0
        } else if self.is_resizable() {
            self.buffer.capacity
        } else {
            self.buffer.data_block.len()
        }
    }

    pub(crate) fn resize(&mut self, new_byte_length: usize) {
        if self.is_resizable() {
            self.buffer.data_block.realloc(new_byte_length);
        } else {
            unreachable!();
        }
    }
}

// SAFETY: Property implemented as a lifetime transmute.
unsafe impl Bindable for ArrayBufferHeapData<'_> {
    type Of<'a> = ArrayBufferHeapData<'a>;

    #[inline(always)]
    fn unbind(self) -> Self::Of<'static> {
        unsafe { core::mem::transmute::<Self, Self::Of<'static>>(self) }
    }

    #[inline(always)]
    fn bind<'a>(self, _gc: NoGcScope<'a, '_>) -> Self::Of<'a> {
        unsafe { core::mem::transmute::<Self, Self::Of<'a>>(self) }
    }
}

impl HeapMarkAndSweep for ArrayBufferHeapData<'static> {
    fn mark_values(&self, queues: &mut WorkQueues) {
        let Self {
            object_index,
            buffer: _,
        } = self;
        object_index.mark_values(queues);
    }

    fn sweep_values(&mut self, compactions: &CompactionLists) {
        let Self {
            object_index,
            buffer: _,
        } = self;
        object_index.sweep_values(compactions);
    }
}
