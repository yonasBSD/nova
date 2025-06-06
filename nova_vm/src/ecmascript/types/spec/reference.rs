// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::ecmascript::abstract_operations::operations_on_objects::{
    private_get, private_set, throw_no_private_name_error, try_private_get, try_set,
};
use crate::ecmascript::execution::agent::JsError;
use crate::ecmascript::types::IntoValue;
use crate::engine::TryResult;
use crate::engine::context::{Bindable, GcScope, NoGcScope};
use crate::engine::rootable::Scopable;
use crate::{
    ecmascript::{
        abstract_operations::{operations_on_objects::set, type_conversion::to_object},
        execution::{
            Environment,
            agent::{self, ExceptionType},
            get_global_object,
        },
        types::{InternalMethods, Object, PropertyKey, String, Value},
    },
    heap::{CompactionLists, HeapMarkAndSweep, WorkQueues},
};
use agent::{Agent, JsResult};

/// ### [6.2.5 The Reference Record Specification Type](https://tc39.es/ecma262/#sec-reference-record-specification-type)
///
/// The Reference Record type is used to explain the behaviour of such
/// operators as delete, typeof, the assignment operators, the super keyword
/// and other language features. For example, the left-hand operand of an
/// assignment is expected to produce a Reference Record.
#[derive(Debug, Clone)]
pub struct Reference<'a> {
    /// ### \[\[Base]]
    ///
    /// The value or Environment Record which holds the binding. A \[\[Base]]
    /// of UNRESOLVABLE indicates that the binding could not be resolved.
    pub(crate) base: Base<'a>,

    /// ### \[\[ReferencedName]]
    ///
    /// The name of the binding. Always a String if \[\[Base]] value is an
    /// Environment Record.
    pub(crate) referenced_name: PropertyKey<'a>,

    /// ### \[\[Strict]]
    ///
    /// true if the Reference Record originated in strict mode code, false
    /// otherwise.
    pub(crate) strict: bool,

    /// ### \[\[ThisValue]]
    ///
    /// If not EMPTY, the Reference Record represents a property binding that
    /// was expressed using the super keyword; it is called a Super Reference
    /// Record and its \[\[Base]] value will never be an Environment Record. In
    /// that case, the \[\[ThisValue]] field holds the this value at the time
    /// the Reference Record was created.
    pub(crate) this_value: Option<Value<'a>>,
}

// SAFETY: Property implemented as a lifetime transmute.
unsafe impl Bindable for Reference<'_> {
    type Of<'a> = Reference<'a>;

    #[inline(always)]
    fn unbind(self) -> Self::Of<'static> {
        unsafe { core::mem::transmute::<Self, Self::Of<'static>>(self) }
    }

    #[inline(always)]
    fn bind<'a>(self, _gc: NoGcScope<'a, '_>) -> Self::Of<'a> {
        unsafe { core::mem::transmute::<Self, Self::Of<'a>>(self) }
    }
}

/// ### [6.2.5.1 IsPropertyReference ( V )](https://tc39.es/ecma262/#sec-ispropertyreference)
///
/// The abstract operation IsPropertyReference takes argument V (a Reference
/// Record) and returns a Boolean.
pub(crate) fn is_property_reference(reference: &Reference) -> bool {
    match reference.base {
        // 1. if V.[[Base]] is unresolvable, return false.
        Base::Unresolvable => false,

        // 2. If V.[[Base]] is an Environment Record, return false; otherwise return true.
        Base::Environment(_) => false,
        _ => true,
    }
}

/// ### [6.2.5.2 IsUnresolvableReference ( V )](https://tc39.es/ecma262/#sec-isunresolvablereference)
///
/// The abstract operation IsUnresolvableReference takes argument V (a
/// Reference Record) and returns a Boolean.
pub(crate) fn is_unresolvable_reference(reference: &Reference) -> bool {
    // 1. If V.[[Base]] is unresolvable, return true; otherwise return false.
    matches!(reference.base, Base::Unresolvable)
}

/// ### [6.2.5.3 IsSuperReference ( V )](https://tc39.es/ecma262/#sec-issuperreference)
///
/// The abstract operation IsSuperReference takes argument V (a Reference
/// Record) and returns a Boolean.
pub(crate) fn is_super_reference(reference: &Reference) -> bool {
    // 1. If V.[[ThisValue]] is not empty, return true; otherwise return false.
    reference.this_value.is_some()
}

/// ### [6.2.5.4 IsPrivateReference ( V )](https://tc39.es/ecma262/#sec-isprivatereference)
///
/// The abstract operation IsPrivateReference takes argument V (a Reference
/// Record) and returns a Boolean.
pub(crate) fn is_private_reference(reference: &Reference) -> bool {
    // 1. If V.[[ReferencedName]] is a Private Name, return true; otherwise return false.
    matches!(reference.referenced_name, PropertyKey::PrivateName(_))
}

/// ### [6.2.5.5 GetValue ( V )](https://tc39.es/ecma262/#sec-getvalue)
/// The abstract operation GetValue takes argument V (a Reference Record or an
/// ECMAScript language value) and returns either a normal completion
/// containing an ECMAScript language value or an abrupt completion.
pub(crate) fn get_value<'gc>(
    agent: &mut Agent,
    reference: &Reference,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let referenced_name = reference.referenced_name.bind(gc.nogc());
    match reference.base {
        Base::Value(value) => {
            // 3. If IsPropertyReference(V) is true, then
            // a. Let baseObj be ? ToObject(V.[[Base]]).

            // NOTE
            // The object that may be created in step 3.a is not
            // accessible outside of the above abstract operation
            // and the ordinary object [[Get]] internal method. An
            // implementation might choose to avoid the actual
            // creation of the object.
            if let Ok(object) = Object::try_from(value) {
                // b. If IsPrivateReference(V) is true, then
                if let PropertyKey::PrivateName(referenced_name) = referenced_name {
                    // i. Return ? PrivateGet(baseObj, V.[[ReferencedName]]).
                    return private_get(agent, object, referenced_name, gc);
                }
                // c. Return ? baseObj.[[Get]](V.[[ReferencedName]], GetThisValue(V)).
                object.internal_get(
                    agent,
                    referenced_name.unbind(),
                    reference.this_value.unwrap_or(object.into_value()),
                    gc,
                )
            } else {
                handle_primitive_get_value(agent, referenced_name.unbind(), value, gc)
            }
        }
        Base::Environment(env) => {
            // 4. Else,
            // a. Let base be V.[[Base]].
            // b. Assert: base is an Environment Record.
            // c. Return ? base.GetBindingValue(V.[[ReferencedName]], V.[[Strict]]) (see 9.1).
            let referenced_name = match &reference.referenced_name {
                PropertyKey::String(data) => String::String(*data),
                PropertyKey::SmallString(data) => String::SmallString(*data),
                _ => unreachable!(),
            };
            Ok(env.get_binding_value(agent, referenced_name, reference.strict, gc)?)
        }
        Base::Unresolvable => {
            // 2. If IsUnresolvableReference(V) is true, throw a ReferenceError exception.
            let error_message = format!(
                "Cannot access undeclared variable '{}'.",
                referenced_name.as_display(agent)
            );
            Err(agent.throw_exception(ExceptionType::ReferenceError, error_message, gc.into_nogc()))
        }
    }
}

fn handle_primitive_get_value<'a>(
    agent: &mut Agent,
    referenced_name: PropertyKey,
    value: Value,
    gc: GcScope<'a, '_>,
) -> JsResult<'a, Value<'a>> {
    // Primitive value. annoying stuff.
    if referenced_name.is_private_name() {
        // i. Return ? PrivateGet(baseObj, V.[[ReferencedName]]).
        return Err(throw_no_private_name_error(agent, gc.into_nogc()));
    }
    match value {
        Value::Undefined | Value::Null => {
            Err(throw_read_undefined_or_null_error(
                agent,
                // SAFETY: We do not care about the conversion validity in
                // error message logging.
                unsafe { referenced_name.into_value_unchecked() },
                value,
                gc.into_nogc(),
            ))
        }
        Value::Boolean(_) => agent
            .current_realm_record()
            .intrinsics()
            .boolean_prototype()
            .internal_get(agent, referenced_name.unbind(), value, gc),
        Value::String(_) | Value::SmallString(_) => {
            let string = String::try_from(value).unwrap();
            if let Some(prop_desc) = string.get_property_descriptor(agent, referenced_name) {
                Ok(prop_desc.value.unwrap())
            } else {
                agent
                    .current_realm_record()
                    .intrinsics()
                    .string_prototype()
                    .internal_get(agent, referenced_name.unbind(), value, gc)
            }
        }
        Value::Symbol(_) => agent
            .current_realm_record()
            .intrinsics()
            .symbol_prototype()
            .internal_get(agent, referenced_name.unbind(), value, gc),
        Value::Number(_) | Value::Integer(_) | Value::SmallF64(_) => agent
            .current_realm_record()
            .intrinsics()
            .number_prototype()
            .internal_get(agent, referenced_name.unbind(), value, gc),
        Value::BigInt(_) | Value::SmallBigInt(_) => agent
            .current_realm_record()
            .intrinsics()
            .big_int_prototype()
            .internal_get(agent, referenced_name.unbind(), value, gc),
        _ => unreachable!(),
    }
}

pub(crate) fn throw_read_undefined_or_null_error<'a>(
    agent: &mut Agent,
    referenced_value: Value,
    value: Value,
    gc: NoGcScope<'a, '_>,
) -> JsError<'a> {
    let error_message = format!(
        "Cannot read property '{}' of {}.",
        referenced_value.try_string_repr(agent, gc).as_str(agent),
        if value.is_undefined() {
            "undefined"
        } else {
            "null"
        }
    );
    agent.throw_exception(ExceptionType::TypeError, error_message, gc)
}

fn try_handle_primitive_get_value<'a>(
    agent: &mut Agent,
    referenced_name: PropertyKey,
    value: Value,
    gc: NoGcScope<'a, '_>,
) -> TryResult<JsResult<'a, Value<'a>>> {
    // b. If IsPrivateReference(V) is true, then
    if referenced_name.is_private_name() {
        // i. Return ? PrivateGet(baseObj, V.[[ReferencedName]]).
        return TryResult::Continue(Err(throw_no_private_name_error(agent, gc)));
    }
    // Primitive value. annoying stuff.
    match value {
        Value::Undefined | Value::Null => {
            TryResult::Continue(Err(throw_read_undefined_or_null_error(
                agent,
                // SAFETY: We do not care about the conversion validity in
                // error message logging.
                unsafe { referenced_name.into_value_unchecked() },
                value,
                gc,
            )))
        }
        Value::Boolean(_) => TryResult::Continue(Ok(agent
            .current_realm_record()
            .intrinsics()
            .boolean_prototype()
            .try_get(agent, referenced_name.unbind(), value, gc)?)),
        Value::String(_) | Value::SmallString(_) => {
            let string = String::try_from(value).unwrap();
            if let Some(prop_desc) = string.get_property_descriptor(agent, referenced_name) {
                TryResult::Continue(Ok(prop_desc.value.unwrap()))
            } else {
                TryResult::Continue(Ok(agent
                    .current_realm_record()
                    .intrinsics()
                    .string_prototype()
                    .try_get(agent, referenced_name.unbind(), value, gc)?))
            }
        }
        Value::Symbol(_) => TryResult::Continue(Ok(agent
            .current_realm_record()
            .intrinsics()
            .symbol_prototype()
            .try_get(agent, referenced_name.unbind(), value, gc)?)),
        Value::Number(_) | Value::Integer(_) | Value::SmallF64(_) => TryResult::Continue(Ok(agent
            .current_realm_record()
            .intrinsics()
            .number_prototype()
            .try_get(agent, referenced_name.unbind(), value, gc)?)),
        Value::BigInt(_) | Value::SmallBigInt(_) => TryResult::Continue(Ok(agent
            .current_realm_record()
            .intrinsics()
            .big_int_prototype()
            .try_get(agent, referenced_name.unbind(), value, gc)?)),
        _ => unreachable!(),
    }
}

/// ### [6.2.5.5 GetValue ( V )](https://tc39.es/ecma262/#sec-getvalue)
/// The abstract operation GetValue takes argument V (a Reference Record or an
/// ECMAScript language value) and returns either a normal completion
/// containing an ECMAScript language value or an abrupt completion.
pub(crate) fn try_get_value<'gc>(
    agent: &mut Agent,
    reference: &Reference,
    gc: NoGcScope<'gc, '_>,
) -> TryResult<JsResult<'gc, Value<'gc>>> {
    let referenced_name = reference.referenced_name.bind(gc);
    match reference.base {
        Base::Value(value) => {
            // 3. If IsPropertyReference(V) is true, then
            // a. Let baseObj be ? ToObject(V.[[Base]]).

            // NOTE
            // The object that may be created in step 3.a is not
            // accessible outside of the above abstract operation
            // and the ordinary object [[Get]] internal method. An
            // implementation might choose to avoid the actual
            // creation of the object.
            if let Ok(object) = Object::try_from(value) {
                // b. If IsPrivateReference(V) is true, then
                if let PropertyKey::PrivateName(referenced_name) = referenced_name {
                    // i. Return ? PrivateGet(baseObj, V.[[ReferencedName]]).
                    return try_private_get(agent, object, referenced_name, gc);
                }
                // c. Return ? baseObj.[[Get]](V.[[ReferencedName]], GetThisValue(V)).
                TryResult::Continue(Ok(object.try_get(
                    agent,
                    referenced_name.unbind(),
                    get_this_value(reference),
                    gc,
                )?))
            } else {
                try_handle_primitive_get_value(agent, referenced_name.unbind(), value, gc)
            }
        }
        Base::Environment(env) => {
            // 4. Else,
            // a. Let base be V.[[Base]].
            // b. Assert: base is an Environment Record.
            // c. Return ? base.GetBindingValue(V.[[ReferencedName]], V.[[Strict]]) (see 9.1).
            let referenced_name = match &reference.referenced_name {
                PropertyKey::String(data) => String::String(*data),
                PropertyKey::SmallString(data) => String::SmallString(*data),
                _ => unreachable!(),
            };
            env.try_get_binding_value(agent, referenced_name, reference.strict, gc)
        }
        Base::Unresolvable => {
            // 2. If IsUnresolvableReference(V) is true, throw a ReferenceError exception.
            let error_message = format!(
                "Cannot access undeclared variable '{}'.",
                referenced_name.as_display(agent)
            );
            TryResult::Continue(Err(agent.throw_exception(
                ExceptionType::ReferenceError,
                error_message,
                gc,
            )))
        }
    }
}

/// ### [6.2.5.6 PutValue ( V, W )](https://tc39.es/ecma262/#sec-putvalue)
///
/// The abstract operation PutValue takes arguments V (a Reference Record or an
/// ECMAScript language value) and W (an ECMAScript language value) and returns
/// either a normal completion containing UNUSED or an abrupt completion.
pub(crate) fn put_value<'a>(
    agent: &mut Agent,
    v: &Reference,
    w: Value,
    mut gc: GcScope<'a, '_>,
) -> JsResult<'a, ()> {
    let w = w.bind(gc.nogc());
    // 1. If V is not a Reference Record, throw a ReferenceError exception.
    // 2. If IsUnresolvableReference(V) is true, then
    if is_unresolvable_reference(v) {
        if v.strict {
            // a. If V.[[Strict]] is true, throw a ReferenceError exception.
            let error_message = format!(
                "Cannot assign to undeclared variable '{}'.",
                v.referenced_name.as_display(agent)
            );
            return Err(agent.throw_exception(
                ExceptionType::ReferenceError,
                error_message,
                gc.into_nogc(),
            ));
        }
        // b. Let globalObj be GetGlobalObject().
        let global_obj = get_global_object(agent, gc.nogc());
        // c. Perform ? Set(globalObj, V.[[ReferencedName]], W, false).
        let referenced_name = v.referenced_name;
        set(
            agent,
            global_obj.unbind(),
            referenced_name,
            w.unbind(),
            false,
            gc,
        )?;
        // d. Return UNUSED.
        Ok(())
    } else if is_property_reference(v) {
        // 3. If IsPropertyReference(V) is true, then
        // a. Let baseObj be ? ToObject(V.[[Base]]).
        let base = match v.base {
            Base::Value(value) => value,
            Base::Environment(_) | Base::Unresolvable => unreachable!(),
        };
        let base_obj = to_object(agent, base, gc.nogc()).unbind()?.bind(gc.nogc());
        // b. If IsPrivateReference(V) is true, then
        let referenced_name = v.referenced_name;
        if let PropertyKey::PrivateName(referenced_name) = referenced_name {
            // i. Return ? PrivateSet(baseObj, V.[[ReferencedName]], W).
            return private_set(
                agent,
                base_obj.unbind(),
                referenced_name.unbind(),
                w.unbind(),
                gc,
            );
        }
        // c. Let succeeded be ? baseObj.[[Set]](V.[[ReferencedName]], W, GetThisValue(V)).
        let this_value = get_this_value(v);
        let scoped_base_obj = base_obj.scope(agent, gc.nogc());
        let succeeded = base_obj
            .unbind()
            .internal_set(
                agent,
                referenced_name,
                w.unbind(),
                this_value,
                gc.reborrow(),
            )
            .unbind()?;
        if !succeeded && v.strict {
            // d. If succeeded is false and V.[[Strict]] is true, throw a TypeError exception.
            let base_obj_repr = scoped_base_obj
                .get(agent)
                .into_value()
                .string_repr(agent, gc.reborrow());
            let error_message = format!(
                "Could not set property '{}' of {}.",
                referenced_name.as_display(agent),
                base_obj_repr.as_str(agent)
            );
            return Err(agent.throw_exception(
                ExceptionType::TypeError,
                error_message,
                gc.into_nogc(),
            ));
        }
        // e. Return UNUSED.
        Ok(())
    } else {
        // 4. Else,
        // a. Let base be V.[[Base]].
        let base = &v.base;
        // b. Assert: base is an Environment Record.
        let Base::Environment(base) = base else {
            unreachable!()
        };
        // c. Return ? base.SetMutableBinding(V.[[ReferencedName]], W, V.[[Strict]]) (see 9.1).
        let referenced_name = match &v.referenced_name {
            PropertyKey::String(data) => String::String(*data),
            PropertyKey::SmallString(data) => String::SmallString(*data),
            _ => unreachable!(),
        };
        base.set_mutable_binding(agent, referenced_name, w.unbind(), v.strict, gc)
    }
    // NOTE
    // The object that may be created in step 3.a is not accessible outside of the above abstract operation and the ordinary object [[Set]] internal method. An implementation might choose to avoid the actual creation of that object.
}

/// ### [6.2.5.6 PutValue ( V, W )](https://tc39.es/ecma262/#sec-putvalue)
///
/// The abstract operation PutValue takes arguments V (a Reference Record or an
/// ECMAScript language value) and W (an ECMAScript language value) and returns
/// either a normal completion containing UNUSED or an abrupt completion.
pub(crate) fn try_put_value<'a>(
    agent: &mut Agent,
    v: &Reference<'a>,
    w: Value,
    gc: NoGcScope<'a, '_>,
) -> TryResult<JsResult<'a, ()>> {
    // 1. If V is not a Reference Record, throw a ReferenceError exception.
    // 2. If IsUnresolvableReference(V) is true, then
    if is_unresolvable_reference(v) {
        if v.strict {
            // a. If V.[[Strict]] is true, throw a ReferenceError exception.
            let error_message = format!(
                "Cannot assign to undeclared variable '{}'.",
                v.referenced_name.as_display(agent)
            );
            return TryResult::Continue(Err(agent.throw_exception(
                ExceptionType::ReferenceError,
                error_message,
                gc,
            )));
        }
        // b. Let globalObj be GetGlobalObject().
        let global_obj = get_global_object(agent, gc);
        // c. Perform ? Set(globalObj, V.[[ReferencedName]], W, false).
        let referenced_name = v.referenced_name;
        if let Err(err) = try_set(agent, global_obj, referenced_name, w, false, gc)? {
            return TryResult::Continue(Err(err));
        };
        // d. Return UNUSED.
        TryResult::Continue(Ok(()))
    } else if is_property_reference(v) {
        // 3. If IsPropertyReference(V) is true, then
        // a. Let baseObj be ? ToObject(V.[[Base]]).
        let base = match v.base {
            Base::Value(value) => value,
            Base::Environment(_) | Base::Unresolvable => unreachable!(),
        };
        let base_obj = match to_object(agent, base, gc) {
            Ok(base_obj) => base_obj,
            Err(err) => return TryResult::Continue(Err(err)),
        };
        // b. If IsPrivateReference(V) is true, then
        if is_private_reference(v) {
            // i. Return ? PrivateSet(baseObj, V.[[ReferencedName]], W).
            todo!();
        }
        // c. Let succeeded be ? baseObj.[[Set]](V.[[ReferencedName]], W, GetThisValue(V)).
        let this_value = get_this_value(v);
        let referenced_name = v.referenced_name;
        let succeeded = base_obj.try_set(agent, referenced_name, w, this_value, gc)?;
        if !succeeded && v.strict {
            // d. If succeeded is false and V.[[Strict]] is true, throw a TypeError exception.
            let base_obj_repr = base_obj.into_value().try_string_repr(agent, gc);
            let error_message = format!(
                "Could not set property '{}' of {}.",
                referenced_name.as_display(agent),
                base_obj_repr.as_str(agent)
            );
            return TryResult::Continue(Err(agent.throw_exception(
                ExceptionType::TypeError,
                error_message,
                gc,
            )));
        }
        // e. Return UNUSED.
        TryResult::Continue(Ok(()))
    } else {
        // 4. Else,
        // a. Let base be V.[[Base]].
        let base = &v.base;
        // b. Assert: base is an Environment Record.
        let Base::Environment(base) = base else {
            unreachable!()
        };
        // c. Return ? base.SetMutableBinding(V.[[ReferencedName]], W, V.[[Strict]]) (see 9.1).
        let referenced_name = match &v.referenced_name {
            PropertyKey::String(data) => String::String(*data),
            PropertyKey::SmallString(data) => String::SmallString(*data),
            _ => unreachable!(),
        };
        base.try_set_mutable_binding(agent, referenced_name, w, v.strict, gc)
    }
    // NOTE
    // The object that may be created in step 3.a is not accessible outside of the above abstract operation and the ordinary object [[Set]] internal method. An implementation might choose to avoid the actual creation of that object.
}

/// ### {6.2.5.8 InitializeReferencedBinding ( V, W )}(https://tc39.es/ecma262/#sec-initializereferencedbinding)
/// The abstract operation InitializeReferencedBinding takes arguments V (a Reference Record) and W
/// (an ECMAScript language value) and returns either a normal completion containing unused or an
/// abrupt completion.
pub(crate) fn initialize_referenced_binding<'a>(
    agent: &mut Agent,
    v: Reference,
    w: Value,
    gc: GcScope<'a, '_>,
) -> JsResult<'a, ()> {
    // 1. Assert: IsUnresolvableReference(V) is false.
    debug_assert!(!is_unresolvable_reference(&v));
    // 2. Let base be V.[[Base]].
    let base = v.base;
    // 3. Assert: base is an Environment Record.
    let Base::Environment(base) = base else {
        unreachable!()
    };
    let referenced_name = match v.referenced_name {
        PropertyKey::String(data) => String::String(data),
        PropertyKey::SmallString(data) => String::SmallString(data),
        _ => unreachable!(),
    };
    // 4. Return ? base.InitializeBinding(V.[[ReferencedName]], W).
    base.initialize_binding(agent, referenced_name.unbind(), w, gc)
}

/// ### {6.2.5.8 InitializeReferencedBinding ( V, W )}(https://tc39.es/ecma262/#sec-initializereferencedbinding)
/// The abstract operation InitializeReferencedBinding takes arguments V (a Reference Record) and W
/// (an ECMAScript language value) and returns either a normal completion containing unused or an
/// abrupt completion.
pub(crate) fn try_initialize_referenced_binding<'a>(
    agent: &mut Agent,
    v: Reference<'a>,
    w: Value,
    gc: NoGcScope<'a, '_>,
) -> TryResult<JsResult<'a, ()>> {
    // 1. Assert: IsUnresolvableReference(V) is false.
    debug_assert!(!is_unresolvable_reference(&v));
    // 2. Let base be V.[[Base]].
    let base = v.base;
    // 3. Assert: base is an Environment Record.
    let Base::Environment(base) = base else {
        unreachable!()
    };
    let referenced_name = match v.referenced_name {
        PropertyKey::String(data) => String::String(data),
        PropertyKey::SmallString(data) => String::SmallString(data),
        _ => unreachable!(),
    };
    // 4. Return ? base.InitializeBinding(V.[[ReferencedName]], W).
    base.try_initialize_binding(agent, referenced_name, w, gc)
}

/// ### {6.2.5.7 GetThisValue ( V )}(https://tc39.es/ecma262/#sec-getthisvalue)
/// The abstract operation GetThisValue takes argument V (a Reference Record)
/// and returns an ECMAScript language value.
pub(crate) fn get_this_value<'a>(reference: &Reference<'a>) -> Value<'a> {
    // 1. Assert: IsPropertyReference(V) is true.
    debug_assert!(is_property_reference(reference));
    // 2. If IsSuperReference(V) is true, return V.[[ThisValue]]; otherwise return V.[[Base]].
    reference
        .this_value
        .unwrap_or_else(|| match reference.base {
            Base::Value(value) => value,
            Base::Environment(_) | Base::Unresolvable => unreachable!(),
        })
}

#[derive(Debug, PartialEq, Clone)]
pub(crate) enum Base<'a> {
    Value(Value<'a>),
    Environment(Environment<'a>),
    Unresolvable,
}

// SAFETY: Property implemented as a lifetime transmute.
unsafe impl Bindable for Base<'_> {
    type Of<'a> = Base<'a>;

    #[inline(always)]
    fn unbind(self) -> Self::Of<'static> {
        unsafe { core::mem::transmute::<Self, Self::Of<'static>>(self) }
    }

    #[inline(always)]
    fn bind<'a>(self, _gc: NoGcScope<'a, '_>) -> Self::Of<'a> {
        unsafe { core::mem::transmute::<Self, Self::Of<'a>>(self) }
    }
}

impl HeapMarkAndSweep for Reference<'static> {
    fn mark_values(&self, queues: &mut WorkQueues) {
        let Self {
            base,
            referenced_name,
            strict: _,
            this_value,
        } = self;
        base.mark_values(queues);
        referenced_name.mark_values(queues);
        this_value.mark_values(queues);
    }

    fn sweep_values(&mut self, compactions: &CompactionLists) {
        let Self {
            base,
            referenced_name,
            strict: _,
            this_value,
        } = self;
        base.sweep_values(compactions);
        referenced_name.sweep_values(compactions);
        this_value.sweep_values(compactions);
    }
}

impl HeapMarkAndSweep for Base<'static> {
    fn mark_values(&self, queues: &mut WorkQueues) {
        match self {
            Base::Value(value) => value.mark_values(queues),
            Base::Environment(idx) => idx.mark_values(queues),
            Base::Unresolvable => {}
        }
    }

    fn sweep_values(&mut self, compactions: &CompactionLists) {
        match self {
            Base::Value(value) => value.sweep_values(compactions),
            Base::Environment(idx) => idx.sweep_values(compactions),
            Base::Unresolvable => {}
        }
    }
}
