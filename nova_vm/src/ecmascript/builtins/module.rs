// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::ops::{Index, IndexMut};
use std::marker::PhantomData;

use crate::engine::context::{Bindable, GcScope, NoGcScope};
use crate::engine::rootable::{HeapRootData, Scopable};
use crate::engine::{TryResult, unwrap_try};
use crate::heap::HeapSweepWeakReference;
use crate::{
    ecmascript::{
        abstract_operations::testing_and_comparison::same_value,
        builtins::ordinary::ordinary_get_own_property,
        execution::{Agent, JsResult, agent::ExceptionType},
        types::{
            InternalMethods, InternalSlots, IntoValue, Object, OrdinaryObject, PropertyDescriptor,
            PropertyKey, String, Value,
        },
    },
    heap::{CompactionLists, HeapMarkAndSweep, WorkQueues},
};

use self::data::ModuleHeapData;

use super::ordinary::{
    ordinary_define_own_property, ordinary_delete, ordinary_get, ordinary_own_property_keys,
    ordinary_try_get, ordinary_try_has_property,
};

pub mod data;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct Module<'a>(u32, PhantomData<&'a ()>);

impl<'a> From<Module<'a>> for Value<'a> {
    fn from(value: Module<'a>) -> Self {
        Value::Module(value)
    }
}

impl<'a> From<Module<'a>> for Object<'a> {
    fn from(value: Module<'a>) -> Self {
        Object::Module(value)
    }
}

impl Index<Module<'_>> for Agent {
    type Output = ModuleHeapData<'static>;

    fn index(&self, index: Module) -> &Self::Output {
        &self.heap.modules[index]
    }
}

impl IndexMut<Module<'_>> for Agent {
    fn index_mut(&mut self, index: Module) -> &mut Self::Output {
        &mut self.heap.modules[index]
    }
}

impl Index<Module<'_>> for Vec<Option<ModuleHeapData<'static>>> {
    type Output = ModuleHeapData<'static>;

    fn index(&self, index: Module) -> &Self::Output {
        self.get(index.get_index())
            .expect("Module out of bounds")
            .as_ref()
            .expect("Module slot empty")
    }
}

impl IndexMut<Module<'_>> for Vec<Option<ModuleHeapData<'static>>> {
    fn index_mut(&mut self, index: Module) -> &mut Self::Output {
        self.get_mut(index.get_index())
            .expect("Module out of bounds")
            .as_mut()
            .expect("Module slot empty")
    }
}

impl Module<'_> {
    pub(crate) const fn _def() -> Self {
        Self::from_u32(0)
    }

    pub(crate) const fn get_index(self) -> usize {
        self.0 as usize
    }

    /// Creates a module identififer from a usize.
    ///
    /// ## Panics
    /// If the given index is greater than `u32::MAX`.
    pub(crate) const fn from_index(value: usize) -> Self {
        assert!(value <= u32::MAX as usize);
        Self(value as u32, PhantomData)
    }

    /// Creates a module identififer from a u32.
    pub(crate) const fn from_u32(value: u32) -> Self {
        Self(value, PhantomData)
    }

    pub(crate) fn last(modules: &[Option<ModuleHeapData>]) -> Self {
        let index = modules.len() - 1;
        Self::from_index(index)
    }
}

// SAFETY: Property implemented as a lifetime transmute.
unsafe impl Bindable for Module<'_> {
    type Of<'a> = Module<'a>;

    #[inline(always)]
    fn unbind(self) -> Self::Of<'static> {
        unsafe { core::mem::transmute::<Self, Self::Of<'static>>(self) }
    }

    #[inline(always)]
    fn bind<'a>(self, _gc: NoGcScope<'a, '_>) -> Self::Of<'a> {
        unsafe { core::mem::transmute::<Self, Self::Of<'a>>(self) }
    }
}

impl<'a> InternalSlots<'a> for Module<'a> {
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

    fn create_backing_object(self, _: &mut Agent) -> OrdinaryObject<'static> {
        unreachable!();
    }

    fn internal_extensible(self, _agent: &Agent) -> bool {
        false
    }

    fn internal_set_extensible(self, _agent: &mut Agent, _value: bool) {}

    fn internal_prototype(self, _agent: &Agent) -> Option<Object<'static>> {
        None
    }

    fn internal_set_prototype(self, _agent: &mut Agent, _prototype: Option<Object>) {}
}

impl<'a> InternalMethods<'a> for Module<'a> {
    /// ### [10.4.6.1 \[\[GetPrototypeOf\]\] ( )](https://tc39.es/ecma262/#sec-module-namespace-exotic-objects-getprototypeof)
    fn try_get_prototype_of<'gc>(
        self,
        _: &mut Agent,
        _: NoGcScope<'gc, '_>,
    ) -> TryResult<Option<Object<'gc>>> {
        TryResult::Continue(None)
    }

    /// ### [10.4.6.2 \[\[SetPrototypeOf\]\] ( V )](https://tc39.es/ecma262/#sec-module-namespace-exotic-objects-setprototypeof-v)
    fn try_set_prototype_of(
        self,
        _: &mut Agent,
        prototype: Option<Object>,
        _: NoGcScope,
    ) -> TryResult<bool> {
        // This is what it all comes down to in the end.
        TryResult::Continue(prototype.is_none())
    }

    /// ### [10.4.6.3 \[\[IsExtensible\]\] ( )](https://tc39.es/ecma262/#sec-module-namespace-exotic-objects-isextensible)
    fn try_is_extensible(self, _: &mut Agent, _: NoGcScope) -> TryResult<bool> {
        TryResult::Continue(false)
    }

    /// ### [10.4.6.4 \[\[PreventExtensions\]\] ( )](https://tc39.es/ecma262/#sec-module-namespace-exotic-objects-preventextensions)
    fn try_prevent_extensions(self, _: &mut Agent, _: NoGcScope) -> TryResult<bool> {
        TryResult::Continue(true)
    }

    fn try_get_own_property<'gc>(
        self,
        agent: &mut Agent,
        property_key: PropertyKey,
        gc: NoGcScope<'gc, '_>,
    ) -> TryResult<Option<PropertyDescriptor<'gc>>> {
        match property_key {
            PropertyKey::Symbol(_) | PropertyKey::PrivateName(_) => {
                // 1. If P is a Symbol, return OrdinaryGetOwnProperty(O, P).
                TryResult::Continue(
                    self.get_backing_object(agent)
                        .and_then(|object| ordinary_get_own_property(agent, object, property_key)),
                )
            }
            PropertyKey::Integer(_) | PropertyKey::SmallString(_) | PropertyKey::String(_) => {
                // 2. Let exports be O.[[Exports]].
                let exports: &[String] = &agent[self].exports;
                let key = match property_key {
                    PropertyKey::SmallString(data) => String::SmallString(data),
                    PropertyKey::String(data) => String::String(data),
                    PropertyKey::Integer(_)
                    | PropertyKey::Symbol(_)
                    | PropertyKey::PrivateName(_) => unreachable!(),
                };
                let exports_contains_p = exports.contains(&key);
                // 3. If exports does not contain P, return undefined.
                if !exports_contains_p {
                    TryResult::Continue(None)
                } else {
                    // 4. Let value be ? O.[[Get]](P, O).
                    let value = self
                        .try_get(agent, property_key, self.into_value(), gc)?
                        .unbind();
                    // 5. Return PropertyDescriptor { [[Value]]: value, [[Writable]]: true, [[Enumerable]]: true, [[Configurable]]: false }.
                    TryResult::Continue(Some(PropertyDescriptor {
                        value: Some(value),
                        writable: Some(true),
                        get: None,
                        set: None,
                        enumerable: Some(true),
                        configurable: Some(false),
                    }))
                }
            }
        }
    }

    /// 10.4.6.5 \[\[GetOwnProperty\]\] ( P )
    fn internal_get_own_property<'gc>(
        self,
        agent: &mut Agent,
        property_key: PropertyKey,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Option<PropertyDescriptor<'gc>>> {
        let property_key = property_key.bind(gc.nogc());
        match property_key {
            PropertyKey::Symbol(_) | PropertyKey::PrivateName(_) => {
                // 1. If P is a Symbol, return OrdinaryGetOwnProperty(O, P).
                Ok(self
                    .get_backing_object(agent)
                    .and_then(|object| ordinary_get_own_property(agent, object, property_key)))
            }
            PropertyKey::Integer(_) | PropertyKey::SmallString(_) | PropertyKey::String(_) => {
                // 2. Let exports be O.[[Exports]].
                let exports: &[String] = &agent[self].exports;
                let key = match property_key {
                    PropertyKey::SmallString(data) => String::SmallString(data),
                    PropertyKey::String(data) => String::String(data),
                    PropertyKey::Integer(_) => todo!(),
                    PropertyKey::Symbol(_) | PropertyKey::PrivateName(_) => unreachable!(),
                };
                let exports_contains_p = exports.contains(&key);
                // 3. If exports does not contain P, return undefined.
                if !exports_contains_p {
                    Ok(None)
                } else {
                    // 4. Let value be ? O.[[Get]](P, O).
                    let value =
                        self.internal_get(agent, property_key.unbind(), self.into_value(), gc)?;
                    // 5. Return PropertyDescriptor { [[Value]]: value, [[Writable]]: true, [[Enumerable]]: true, [[Configurable]]: false }.
                    Ok(Some(PropertyDescriptor {
                        value: Some(value.unbind()),
                        writable: Some(true),
                        get: None,
                        set: None,
                        enumerable: Some(true),
                        configurable: Some(false),
                    }))
                }
            }
        }
    }

    /// ### [10.4.6.6 \[\[DefineOwnProperty\]\] ( P, Desc )](https://tc39.es/ecma262/#sec-module-namespace-exotic-objects-defineownproperty-p-desc)
    fn try_define_own_property(
        self,
        agent: &mut Agent,
        property_key: PropertyKey,
        property_descriptor: PropertyDescriptor,
        gc: NoGcScope,
    ) -> TryResult<bool> {
        match property_key {
            PropertyKey::Symbol(_) | PropertyKey::PrivateName(_) => {
                // 1. If P is a Symbol, return ! OrdinaryDefineOwnProperty(O, P, Desc).
                TryResult::Continue(self.get_backing_object(agent).is_some_and(|object| {
                    ordinary_define_own_property(
                        agent,
                        object,
                        property_key,
                        property_descriptor,
                        gc,
                    )
                }))
            }
            PropertyKey::Integer(_) | PropertyKey::SmallString(_) | PropertyKey::String(_) => {
                // 2. Let current be ? O.[[GetOwnProperty]](P).
                let current = self.try_get_own_property(agent, property_key, gc)?;
                // 3. If current is undefined, return false.
                let Some(current) = current else {
                    return TryResult::Continue(false);
                };
                // 4. If Desc has a [[Configurable]] field and Desc.[[Configurable]] is true, return false.
                if property_descriptor.configurable == Some(true) {
                    return TryResult::Continue(false);
                }
                // 5. If Desc has an [[Enumerable]] field and Desc.[[Enumerable]] is false, return false.
                if property_descriptor.enumerable == Some(false) {
                    return TryResult::Continue(false);
                }
                // 6. If IsAccessorDescriptor(Desc) is true, return false.
                if property_descriptor.is_accessor_descriptor() {
                    return TryResult::Continue(false);
                }
                // 7. If Desc has a [[Writable]] field and Desc.[[Writable]] is false, return false.
                if property_descriptor.writable == Some(false) {
                    return TryResult::Continue(false);
                }
                // 8. If Desc has a [[Value]] field, return SameValue(Desc.[[Value]], current.[[Value]]).
                if let Some(value) = property_descriptor.value {
                    TryResult::Continue(same_value(agent, value, current.value.unwrap()))
                } else {
                    // 9. Return true.
                    TryResult::Continue(true)
                }
            }
        }
    }

    /// ### [10.4.6.6 \[\[DefineOwnProperty\]\] ( P, Desc )](https://tc39.es/ecma262/#sec-module-namespace-exotic-objects-defineownproperty-p-desc)
    fn internal_define_own_property<'gc>(
        self,
        agent: &mut Agent,
        property_key: PropertyKey,
        property_descriptor: PropertyDescriptor,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, bool> {
        let o = self.bind(gc.nogc());
        let property_key = property_key.bind(gc.nogc());
        let property_descriptor = property_descriptor.bind(gc.nogc());
        match property_key {
            PropertyKey::Symbol(_) | PropertyKey::PrivateName(_) => {
                // 1. If P is a Symbol, return ! OrdinaryDefineOwnProperty(O, P, Desc).
                Ok(match o.get_backing_object(agent) {
                    Some(object) => ordinary_define_own_property(
                        agent,
                        object,
                        property_key.unbind(),
                        property_descriptor.unbind(),
                        gc.into_nogc(),
                    ),
                    None => false,
                })
            }
            PropertyKey::Integer(_) | PropertyKey::SmallString(_) | PropertyKey::String(_) => {
                // 2. Let current be ? O.[[GetOwnProperty]](P).
                let is_accessor_descriptor = property_descriptor.is_accessor_descriptor();
                let PropertyDescriptor {
                    value,
                    writable,
                    enumerable,
                    configurable,
                    ..
                } = property_descriptor;
                let value = value.map(|v| v.scope(agent, gc.nogc()));
                let current =
                    o.unbind()
                        .internal_get_own_property(agent, property_key.unbind(), gc)?;
                // 3. If current is undefined, return false.
                let Some(current) = current else {
                    return Ok(false);
                };
                // 4. If Desc has a [[Configurable]] field and Desc.[[Configurable]] is true, return false.
                if configurable == Some(true) {
                    return Ok(false);
                }
                // 5. If Desc has an [[Enumerable]] field and Desc.[[Enumerable]] is false, return false.
                if enumerable == Some(false) {
                    return Ok(false);
                }
                // 6. If IsAccessorDescriptor(Desc) is true, return false.
                if is_accessor_descriptor {
                    return Ok(false);
                }
                // 7. If Desc has a [[Writable]] field and Desc.[[Writable]] is false, return false.
                if writable == Some(false) {
                    return Ok(false);
                }
                // 8. If Desc has a [[Value]] field, return SameValue(Desc.[[Value]], current.[[Value]]).
                if let Some(value) = value {
                    Ok(same_value(
                        agent,
                        value.get(agent),
                        current.value.unwrap_or(Value::Undefined),
                    ))
                } else {
                    // 9. Return true.
                    Ok(true)
                }
            }
        }
    }

    /// ### [10.4.6.7 \[\[HasProperty\]\] ( P )](https://tc39.es/ecma262/#sec-module-namespace-exotic-objects-hasproperty-p)
    fn try_has_property(
        self,
        agent: &mut Agent,
        property_key: PropertyKey,
        gc: NoGcScope,
    ) -> TryResult<bool> {
        match property_key {
            PropertyKey::Integer(_) | PropertyKey::SmallString(_) | PropertyKey::String(_) => {
                let p = match property_key {
                    PropertyKey::String(data) => String::String(data),
                    PropertyKey::SmallString(data) => String::SmallString(data),
                    PropertyKey::Integer(_data) => todo!(),
                    _ => unreachable!(),
                };
                // 2. Let exports be O.[[Exports]].
                let exports: &[String] = &agent[self].exports;
                // 3. If exports contains P, return true.
                if exports.contains(&p) {
                    TryResult::Continue(true)
                } else {
                    // 4. Return false.
                    TryResult::Continue(false)
                }
            }
            PropertyKey::Symbol(_) | PropertyKey::PrivateName(_) => {
                // 1. If P is a Symbol, return ! OrdinaryHasProperty(O, P).
                TryResult::Continue(self.get_backing_object(agent).is_some_and(|object| {
                    unwrap_try(ordinary_try_has_property(agent, object, property_key, gc))
                }))
            }
        }
    }

    /// ### [10.4.6.8 \[\[Get\]\] ( P, Receiver )](https://tc39.es/ecma262/#sec-module-namespace-exotic-objects-get-p-receiver)
    fn try_get<'gc>(
        self,
        agent: &mut Agent,
        property_key: PropertyKey,
        receiver: Value,
        gc: NoGcScope<'gc, '_>,
    ) -> TryResult<Value<'gc>> {
        // NOTE: ResolveExport is side-effect free. Each time this operation
        // is called with a specific exportName, resolveSet pair as arguments
        // it must return the same result. An implementation might choose to
        // pre-compute or cache the ResolveExport results for the [[Exports]]
        // of each module namespace exotic object.

        match property_key {
            // 1. If P is a Symbol, then
            PropertyKey::Symbol(_) | PropertyKey::PrivateName(_) => {
                // a. Return ! OrdinaryGet(O, P, Receiver).
                TryResult::Continue(self.get_backing_object(agent).map_or(
                    Value::Undefined,
                    |object| {
                        unwrap_try(ordinary_try_get(agent, object, property_key, receiver, gc))
                    },
                ))
            }
            PropertyKey::Integer(_) | PropertyKey::SmallString(_) | PropertyKey::String(_) => {
                // 2. Let exports be O.[[Exports]].
                let exports: &[String] = &agent[self].exports;
                let key = match property_key {
                    PropertyKey::SmallString(data) => String::SmallString(data),
                    PropertyKey::String(data) => String::String(data),
                    PropertyKey::Integer(_) => todo!(),
                    PropertyKey::Symbol(_) | PropertyKey::PrivateName(_) => unreachable!(),
                };
                let exports_contains_p = exports.contains(&key);
                // 3. If exports does not contain P, return undefined.
                if !exports_contains_p {
                    TryResult::Continue(Value::Undefined)
                } else {
                    // 4. Let m be O.[[Module]].
                    let m = &agent[self].module;
                    // 5. Let binding be m.ResolveExport(P).
                    let binding = m.resolve_export(property_key);
                    // 6. Assert: binding is a ResolvedBinding Record.
                    let Some(data::ResolveExportResult::Resolved(binding)) = binding else {
                        unreachable!();
                    };
                    // 7. Let targetModule be binding.[[Module]].
                    // 8. Assert: targetModule is not undefined.
                    let target_module = binding.module.unwrap();
                    // 9. If binding.[[BindingName]] is NAMESPACE, then
                    let _binding_name = match binding.binding_name {
                        data::ResolvedBindingName::Namespace => {
                            // a. Return GetModuleNamespace(targetModule).
                            todo!();
                        }
                        data::ResolvedBindingName::String(data) => String::String(data),
                        data::ResolvedBindingName::SmallString(data) => String::SmallString(data),
                    };
                    // 10. Let targetEnv be targetModule.[[Environment]].
                    let target_env = agent[target_module].module.environment;
                    // 11. If targetEnv is EMPTY, throw a ReferenceError exception.
                    match target_env {
                        None => TryResult::Break(()),
                        Some(_target_env) => {
                            // 12. Return ? targetEnv.GetBindingValue(binding.[[BindingName]], true).
                            todo!()
                        }
                    }
                }
            }
        }
    }

    /// ### [10.4.6.8 \[\[Get\]\] ( P, Receiver )](https://tc39.es/ecma262/#sec-module-namespace-exotic-objects-get-p-receiver)
    fn internal_get<'gc>(
        self,
        agent: &mut Agent,
        property_key: PropertyKey,
        receiver: Value,
        mut gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let property_key = property_key.bind(gc.nogc());

        // NOTE: ResolveExport is side-effect free. Each time this operation
        // is called with a specific exportName, resolveSet pair as arguments
        // it must return the same result. An implementation might choose to
        // pre-compute or cache the ResolveExport results for the [[Exports]]
        // of each module namespace exotic object.

        match property_key {
            // 1. If P is a Symbol, then
            PropertyKey::Symbol(_) | PropertyKey::PrivateName(_) => {
                // a. Return ! OrdinaryGet(O, P, Receiver).
                Ok(match self.get_backing_object(agent) {
                    Some(object) => ordinary_get(
                        agent,
                        object,
                        property_key.unbind(),
                        receiver,
                        gc.reborrow(),
                    )
                    .unwrap()
                    .unbind(),
                    None => Value::Undefined,
                })
            }
            PropertyKey::Integer(_) | PropertyKey::SmallString(_) | PropertyKey::String(_) => {
                // 2. Let exports be O.[[Exports]].
                let exports: &[String] = &agent[self].exports;
                let key = match property_key {
                    PropertyKey::SmallString(data) => String::SmallString(data),
                    PropertyKey::String(data) => String::String(data),
                    PropertyKey::Integer(_) => todo!(),
                    _ => unreachable!(),
                };
                let exports_contains_p = exports.contains(&key);
                // 3. If exports does not contain P, return undefined.
                if !exports_contains_p {
                    Ok(Value::Undefined)
                } else {
                    // 4. Let m be O.[[Module]].
                    let m = &agent[self].module;
                    // 5. Let binding be m.ResolveExport(P).
                    let binding = m.resolve_export(property_key);
                    // 6. Assert: binding is a ResolvedBinding Record.
                    let Some(data::ResolveExportResult::Resolved(binding)) = binding else {
                        unreachable!();
                    };
                    // 7. Let targetModule be binding.[[Module]].
                    // 8. Assert: targetModule is not undefined.
                    let target_module = binding.module.unwrap();
                    // 9. If binding.[[BindingName]] is NAMESPACE, then
                    let _binding_name = match binding.binding_name {
                        data::ResolvedBindingName::Namespace => {
                            // a. Return GetModuleNamespace(targetModule).
                            todo!();
                        }
                        data::ResolvedBindingName::String(data) => String::String(data),
                        data::ResolvedBindingName::SmallString(data) => String::SmallString(data),
                    };
                    // 10. Let targetEnv be targetModule.[[Environment]].
                    let target_env = agent[target_module].module.environment;
                    // 11. If targetEnv is EMPTY, throw a ReferenceError exception.
                    match target_env {
                        None => Err(agent.throw_exception(
                            ExceptionType::ReferenceError,
                            format!("Could not resolve module '{}'.", key.as_str(agent)),
                            gc.into_nogc(),
                        )),
                        Some(_target_env) => {
                            // 12. Return ? targetEnv.GetBindingValue(binding.[[BindingName]], true).
                            todo!()
                        }
                    }
                }
            }
        }
    }

    /// ### [10.4.6.9 \[\[Set\]\] ( P, V, Receiver )](https://tc39.es/ecma262/#sec-module-namespace-exotic-objects-set-p-v-receiver)
    fn try_set(
        self,
        _: &mut Agent,
        _: PropertyKey,
        _: Value,
        _: Value,
        _: NoGcScope,
    ) -> TryResult<bool> {
        TryResult::Continue(false)
    }

    /// ### [10.4.6.10 \[\[Delete\]\] ( P )](https://tc39.es/ecma262/#sec-module-namespace-exotic-objects-delete-p)
    fn try_delete(
        self,
        agent: &mut Agent,
        property_key: PropertyKey,
        gc: NoGcScope,
    ) -> TryResult<bool> {
        match property_key {
            PropertyKey::Symbol(_) | PropertyKey::PrivateName(_) => {
                // 1. If P is a Symbol, then
                // a. Return ! OrdinaryDelete(O, P).
                TryResult::Continue(
                    self.get_backing_object(agent)
                        .is_none_or(|object| ordinary_delete(agent, object, property_key, gc)),
                )
            }
            PropertyKey::Integer(_) | PropertyKey::SmallString(_) | PropertyKey::String(_) => {
                let p = match property_key {
                    PropertyKey::String(data) => String::String(data),
                    PropertyKey::SmallString(data) => String::SmallString(data),
                    PropertyKey::Integer(_) => todo!(),
                    _ => unreachable!(),
                };
                // 2. Let exports be O.[[Exports]].
                let exports = &agent[self].exports;
                // 3. If exports contains P, return false.
                if exports.contains(&p) {
                    TryResult::Continue(false)
                } else {
                    // 4. Return true.
                    TryResult::Continue(true)
                }
            }
        }
    }

    /// ### [10.4.6.11 \[\[OwnPropertyKeys\]\] ( )])(https://tc39.es/ecma262/#sec-module-namespace-exotic-objects-ownpropertykeys)
    fn try_own_property_keys<'gc>(
        self,
        agent: &mut Agent,
        gc: NoGcScope<'gc, '_>,
    ) -> TryResult<Vec<PropertyKey<'gc>>> {
        // 1. Let exports be O.[[Exports]].
        let exports = agent[self]
            .exports
            .iter()
            .map(|string| PropertyKey::from(*string));
        let exports_count = exports.len();
        // 2. Let symbolKeys be OrdinaryOwnPropertyKeys(O).
        let symbol_keys = self.get_backing_object(agent).map_or(vec![], |object| {
            ordinary_own_property_keys(agent, object, gc)
        });
        let symbol_keys_count = symbol_keys.len();
        // 3. Return the list-concatenation of exports and symbolKeys.
        let mut own_property_keys = Vec::with_capacity(exports_count + symbol_keys_count);
        exports.for_each(|export_key| own_property_keys.push(export_key));
        symbol_keys
            .iter()
            .for_each(|symbol_key| own_property_keys.push(*symbol_key));
        TryResult::Continue(own_property_keys)
    }
}

impl TryFrom<HeapRootData> for Module<'_> {
    type Error = ();

    #[inline]
    fn try_from(value: HeapRootData) -> Result<Self, Self::Error> {
        if let HeapRootData::Module(value) = value {
            Ok(value)
        } else {
            Err(())
        }
    }
}

impl HeapMarkAndSweep for Module<'static> {
    fn mark_values(&self, queues: &mut WorkQueues) {
        queues.modules.push(*self);
    }

    fn sweep_values(&mut self, compactions: &CompactionLists) {
        compactions.modules.shift_u32_index(&mut self.0);
    }
}

impl HeapSweepWeakReference for Module<'static> {
    fn sweep_weak_reference(self, compactions: &CompactionLists) -> Option<Self> {
        compactions
            .modules
            .shift_weak_u32_index(self.0)
            .map(Self::from_u32)
    }
}
