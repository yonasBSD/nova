// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::ops::{Index, IndexMut};

use crate::{
    ecmascript::{
        builtins::Array,
        execution::{Agent, ProtoIntrinsics},
        types::{InternalMethods, InternalSlots, IntoObject, Object, OrdinaryObject, Value},
    },
    engine::{
        context::{Bindable, NoGcScope},
        rootable::HeapRootData,
    },
    heap::{
        CompactionLists, CreateHeapData, Heap, HeapMarkAndSweep, HeapSweepWeakReference,
        WorkQueues, indexes::ArrayIteratorIndex,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ArrayIterator<'a>(ArrayIteratorIndex<'a>);

impl<'a> ArrayIterator<'a> {
    /// # Do not use this
    /// This is only for Value discriminant creation.
    pub(crate) const fn _def() -> Self {
        Self(ArrayIteratorIndex::from_u32_index(0))
    }

    pub(crate) fn get_index(self) -> usize {
        self.0.into_index()
    }

    pub(crate) fn from_object(
        agent: &mut Agent,
        array: Object,
        kind: CollectionIteratorKind,
    ) -> Self {
        agent.heap.create(ArrayIteratorHeapData {
            object_index: None,
            array: Some(array.unbind()),
            next_index: 0,
            kind,
        })
    }

    pub(crate) fn from_vm_iterator(
        agent: &mut Agent,
        array: Array,
        index: u32,
        gc: NoGcScope<'a, '_>,
    ) -> Self {
        agent
            .heap
            .create(ArrayIteratorHeapData {
                object_index: None,
                array: Some(array.into_object().unbind()),
                next_index: index as i64,
                kind: CollectionIteratorKind::Value,
            })
            .bind(gc)
    }
}

// SAFETY: Property implemented as a lifetime transmute.
unsafe impl Bindable for ArrayIterator<'_> {
    type Of<'a> = ArrayIterator<'a>;

    #[inline(always)]
    fn unbind(self) -> Self::Of<'static> {
        unsafe { core::mem::transmute::<Self, Self::Of<'static>>(self) }
    }

    #[inline(always)]
    fn bind<'a>(self, _gc: NoGcScope<'a, '_>) -> Self::Of<'a> {
        unsafe { core::mem::transmute::<Self, Self::Of<'a>>(self) }
    }
}

impl<'a> From<ArrayIterator<'a>> for Object<'a> {
    fn from(value: ArrayIterator) -> Self {
        Self::ArrayIterator(value.unbind())
    }
}

impl<'a> From<ArrayIterator<'a>> for Value<'a> {
    fn from(value: ArrayIterator<'a>) -> Self {
        Self::ArrayIterator(value)
    }
}

impl<'a> TryFrom<Value<'a>> for ArrayIterator<'a> {
    type Error = ();

    fn try_from(value: Value<'a>) -> Result<Self, Self::Error> {
        match value {
            Value::ArrayIterator(data) => Ok(data),
            _ => Err(()),
        }
    }
}

impl<'a> TryFrom<Object<'a>> for ArrayIterator<'a> {
    type Error = ();

    fn try_from(value: Object<'a>) -> Result<Self, Self::Error> {
        match value {
            Object::ArrayIterator(data) => Ok(data),
            _ => Err(()),
        }
    }
}

impl<'a> InternalSlots<'a> for ArrayIterator<'a> {
    const DEFAULT_PROTOTYPE: ProtoIntrinsics = ProtoIntrinsics::ArrayIterator;

    #[inline(always)]
    fn get_backing_object(self, agent: &Agent) -> Option<OrdinaryObject<'static>> {
        agent[self].object_index
    }

    fn set_backing_object(self, agent: &mut Agent, backing_object: OrdinaryObject<'static>) {
        assert!(
            agent[self]
                .object_index
                .replace(backing_object.unbind())
                .is_none()
        );
    }
}

impl<'a> InternalMethods<'a> for ArrayIterator<'a> {}

impl Index<ArrayIterator<'_>> for Agent {
    type Output = ArrayIteratorHeapData<'static>;

    fn index(&self, index: ArrayIterator) -> &Self::Output {
        &self.heap.array_iterators[index]
    }
}

impl IndexMut<ArrayIterator<'_>> for Agent {
    fn index_mut(&mut self, index: ArrayIterator) -> &mut Self::Output {
        &mut self.heap.array_iterators[index]
    }
}

impl Index<ArrayIterator<'_>> for Vec<Option<ArrayIteratorHeapData<'static>>> {
    type Output = ArrayIteratorHeapData<'static>;

    fn index(&self, index: ArrayIterator) -> &Self::Output {
        self.get(index.get_index())
            .expect("ArrayIterator out of bounds")
            .as_ref()
            .expect("Array ArrayIterator empty")
    }
}

impl IndexMut<ArrayIterator<'_>> for Vec<Option<ArrayIteratorHeapData<'static>>> {
    fn index_mut(&mut self, index: ArrayIterator) -> &mut Self::Output {
        self.get_mut(index.get_index())
            .expect("ArrayIterator out of bounds")
            .as_mut()
            .expect("ArrayIterator slot empty")
    }
}

impl TryFrom<HeapRootData> for ArrayIterator<'_> {
    type Error = ();

    #[inline]
    fn try_from(value: HeapRootData) -> Result<Self, Self::Error> {
        if let HeapRootData::ArrayIterator(value) = value {
            Ok(value)
        } else {
            Err(())
        }
    }
}

impl<'a> CreateHeapData<ArrayIteratorHeapData<'a>, ArrayIterator<'a>> for Heap {
    fn create(&mut self, data: ArrayIteratorHeapData<'a>) -> ArrayIterator<'a> {
        self.array_iterators.push(Some(data.unbind()));
        self.alloc_counter += core::mem::size_of::<Option<ArrayIteratorHeapData<'static>>>();
        ArrayIterator(ArrayIteratorIndex::last(&self.array_iterators))
    }
}

impl HeapMarkAndSweep for ArrayIterator<'static> {
    fn mark_values(&self, queues: &mut WorkQueues) {
        queues.array_iterators.push(*self);
    }

    fn sweep_values(&mut self, compactions: &CompactionLists) {
        compactions.array_iterators.shift_index(&mut self.0);
    }
}

impl HeapSweepWeakReference for ArrayIterator<'static> {
    fn sweep_weak_reference(self, compactions: &CompactionLists) -> Option<Self> {
        compactions
            .array_iterators
            .shift_weak_index(self.0)
            .map(Self)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) enum CollectionIteratorKind {
    #[default]
    Key,
    Value,
    KeyAndValue,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ArrayIteratorHeapData<'a> {
    pub(crate) object_index: Option<OrdinaryObject<'a>>,
    pub(crate) array: Option<Object<'a>>,
    pub(crate) next_index: i64,
    pub(crate) kind: CollectionIteratorKind,
}

// SAFETY: Property implemented as a lifetime transmute.
unsafe impl Bindable for ArrayIteratorHeapData<'_> {
    type Of<'a> = ArrayIteratorHeapData<'a>;

    #[inline(always)]
    fn unbind(self) -> Self::Of<'static> {
        unsafe { core::mem::transmute::<Self, Self::Of<'static>>(self) }
    }

    #[inline(always)]
    fn bind<'a>(self, _gc: NoGcScope<'a, '_>) -> Self::Of<'a> {
        unsafe { core::mem::transmute::<Self, Self::Of<'a>>(self) }
    }
}

impl HeapMarkAndSweep for ArrayIteratorHeapData<'static> {
    fn mark_values(&self, queues: &mut WorkQueues) {
        let Self {
            object_index,
            array,
            next_index: _,
            kind: _,
        } = self;
        object_index.mark_values(queues);
        array.mark_values(queues);
    }

    fn sweep_values(&mut self, compactions: &CompactionLists) {
        let Self {
            object_index,
            array,
            next_index: _,
            kind: _,
        } = self;
        object_index.sweep_values(compactions);
        array.sweep_values(compactions);
    }
}
