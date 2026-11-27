//! Classes defined in Rust: a payload `T` in each instance, a prototype of methods and accessors, a constructor.
//!
//! The payload is a cell of its own, apart from the object's properties: a member converts its arguments (which may
//! run script, and script may touch the object) before it borrows the payload, and borrows it only for the body. A
//! body that runs script which re-enters a member of the same object while it borrows the payload mutably makes that
//! member throw a `TypeError`, where JavaScriptCore would let it alias.
//!
//! ```ignore
//! #[derive(Trace, Finalize)]
//! struct Counter { count: u32 }
//! impl JsClass for Counter {
//!     const NAME: &'static str = "Counter";
//!     fn construct(_: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<Self> { Ok(Counter { count: 0 }) }
//!     fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
//!         class.getter("count", Attributes::DEFAULT, |this, global| Ok(JSValue::js_number(this.borrow(global)?.count.into())))?;
//!         class.method("increment", 0, Attributes::DEFAULT, |this, global, _| {
//!             this.borrow_mut(global)?.count += 1;
//!             Ok(JSValue::UNDEFINED)
//!         })
//!     }
//! }
//! ```

use core::any::TypeId;
use core::marker::PhantomData;

use boa_engine::object::builtins::JsFunction;
use boa_engine::object::{FunctionObjectBuilder, JsData};
use boa_engine::property::PropertyDescriptor;
use boa_engine::{Context, JsObject, JsString, JsSymbol, JsValue, NativeFunction};
use boa_gc::{Finalize, Gc, GcRef, GcRefCell, GcRefMut, Trace, custom_trace};

use crate::host_fn::{call_host_fn, native_function};
use crate::{CallFrame, ErrorCode, JSGlobalObject, JSHostFn, JSValue, JsResult};

/// A Rust type whose values are the payloads of JavaScript objects. It is traced by Boa's collector: a JS value it
/// holds is a field that `Trace` visits.
pub trait JsClass: Trace + Finalize + Sized + 'static {
    const NAME: &'static str;
    /// The constructor's `length`.
    const LENGTH: usize = 0;

    /// `new T(...)`. By default JavaScript cannot construct one, only Rust (`to_js`).
    fn construct(global: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<Self> {
        Err(ErrorCode::ILLEGAL_CONSTRUCTOR.throw(global, format_args!("Illegal constructor")))
    }

    /// After `construct`, on the new instance: the rest of a constructor that needs the object itself, as a
    /// stream's that gives its controller the stream. An exception discards the instance.
    fn initialize(_this: &ClassRef<Self>, _global: &JSGlobalObject, _frame: &CallFrame<'_>) -> JsResult<()> {
        Ok(())
    }

    /// Adds the methods, accessors and static members.
    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()>;
}

/// What the object holds: the payload's own cell.
struct Payload<T: Trace + 'static>(Gc<GcRefCell<T>>);

// SAFETY: the one field is traced.
unsafe impl<T: Trace + 'static> Trace for Payload<T> {
    custom_trace!(this, mark, mark(&this.0));
}

impl<T: Trace + 'static> Finalize for Payload<T> {}

impl<T: Trace + 'static> JsData for Payload<T> {}

/// An instance of `T`, which keeps it alive. Members get one for `this`.
pub struct ClassRef<T: Trace + 'static> {
    object: JsObject,
    payload: Gc<GcRefCell<T>>,
}

impl<T: Trace + 'static> Clone for ClassRef<T> {
    fn clone(&self) -> Self {
        ClassRef {
            object: self.object.clone(),
            payload: self.payload.clone(),
        }
    }
}

impl<T: JsClass> ClassRef<T> {
    fn of(object: &JsObject) -> Option<ClassRef<T>> {
        let payload = object.downcast_ref::<Payload<T>>()?.0.clone();
        Some(ClassRef {
            object: object.clone(),
            payload,
        })
    }

    /// The payload, shared while the `GcRef` lives. A `TypeError` while a call borrows it mutably.
    pub fn borrow(&self, global: &JSGlobalObject) -> JsResult<GcRef<'_, T>> {
        self.payload.try_borrow().map_err(|_| in_use::<T>(global))
    }

    /// The payload, exclusively while the `GcRefMut` lives. A `TypeError` while a call borrows it.
    pub fn borrow_mut(&self, global: &JSGlobalObject) -> JsResult<GcRefMut<'_, T>> {
        self.payload
            .try_borrow_mut()
            .map_err(|_| in_use::<T>(global))
    }

    pub fn to_js(&self) -> JSValue {
        JSValue(self.object.clone().into())
    }
}

fn in_use<T: JsClass>(global: &JSGlobalObject) -> crate::JsError {
    global.throw_type_error(format_args!(
        "This {} is in use by a call that has not returned",
        T::NAME
    ))
}

impl JSValue {
    /// The instance of `T` this is, if it is one.
    pub fn as_class_ref<T: JsClass>(&self) -> Option<ClassRef<T>> {
        ClassRef::of(&self.0.as_object()?)
    }
}

/// A member's property attributes. The default is the bindgen's for a class that is not `webIDL`.
#[derive(Clone, Copy, Debug)]
pub struct Attributes {
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
}

impl Attributes {
    pub const DEFAULT: Attributes = Attributes {
        writable: true,
        enumerable: true,
        configurable: false,
    };
}

pub type Method<T> = fn(&ClassRef<T>, &JSGlobalObject, &CallFrame<'_>) -> JsResult<JSValue>;
pub type Getter<T> = fn(&ClassRef<T>, &JSGlobalObject) -> JsResult<JSValue>;
pub type Setter<T> = fn(&ClassRef<T>, &JSGlobalObject, JSValue) -> JsResult<()>;

/// What `JsClass::define` adds members with.
pub struct ClassBuilder<'g, T> {
    global: &'g JSGlobalObject,
    prototype: JsObject,
    constructor: JsObject,
    _payload: PhantomData<fn() -> T>,
}

/// `ERR_INVALID_THIS`, as C++'s `Bun::throwInvalidThisError`: `Expected this to be instanceof Foo, but received …`.
fn invalid_this<T: JsClass>(global: &JSGlobalObject, this: &JSValue) -> crate::JsError {
    if this.is_undefined() {
        return ErrorCode::INVALID_THIS.throw(
            global,
            format_args!("Expected this to be instanceof {}", T::NAME),
        );
    }
    match crate::bindgen::determine_specific_type(global, this) {
        Ok(received) => ErrorCode::INVALID_THIS.throw(
            global,
            format_args!(
                "Expected this to be instanceof {}, but received {received}",
                T::NAME
            ),
        ),
        Err(error) => error,
    }
}

fn this_of<T: JsClass>(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<ClassRef<T>> {
    let this = frame.this();
    this.as_class_ref::<T>()
        .ok_or_else(|| invalid_this::<T>(global, this))
}

/// What JavaScriptCore throws for a `DOMAttribute` accessor, which a bindgen accessor is, on another object.
fn accessor_this_of<T: JsClass>(
    global: &JSGlobalObject,
    frame: &CallFrame<'_>,
    accessor: &str,
) -> JsResult<ClassRef<T>> {
    frame.this().as_class_ref::<T>().ok_or_else(|| {
        global.throw_type_error(format_args!(
            "The {}.{accessor} can only be used on instances of {}",
            T::NAME,
            T::NAME
        ))
    })
}

impl<'g, T: JsClass> ClassBuilder<'g, T> {
    pub fn global(&self) -> &'g JSGlobalObject {
        self.global
    }

    fn function(&self, name: &str, length: usize, function: NativeFunction) -> JsFunction {
        FunctionObjectBuilder::new(self.global.cx().realm(), function)
            .name(JsString::from(name))
            .length(length)
            .build()
    }

    fn define_on(
        &self,
        target: &JsObject,
        name: &str,
        descriptor: PropertyDescriptor,
    ) -> JsResult<()> {
        let result =
            target.define_property_or_throw(JsString::from(name), descriptor, self.global.cx());
        self.global.lift(result).map(drop)
    }

    fn value_descriptor(value: JsValue, attributes: Attributes) -> PropertyDescriptor {
        PropertyDescriptor::builder()
            .value(value)
            .writable(attributes.writable)
            .enumerable(attributes.enumerable)
            .configurable(attributes.configurable)
            .build()
    }

    /// A method on the prototype.
    pub fn method(
        &mut self,
        name: &'static str,
        length: usize,
        attributes: Attributes,
        method: Method<T>,
    ) -> JsResult<()> {
        let function = self.function(
            name,
            length,
            NativeFunction::from_copy_closure(move |this, args, context| {
                call_host_fn(
                    |global, frame| method(&this_of::<T>(global, frame)?, global, frame),
                    this,
                    args,
                    context,
                )
            }),
        );
        self.define_on(
            &self.prototype,
            name,
            Self::value_descriptor(function.into(), attributes),
        )
    }

    /// A function on the prototype that finds its payload itself (`frame.this()`), for a class whose methods also
    /// take an object that is not an instance, as C++ that downcasts `this` its own way does.
    pub fn prototype_function(
        &mut self,
        name: &'static str,
        length: usize,
        attributes: Attributes,
        f: JSHostFn,
    ) -> JsResult<()> {
        let function = self.function(name, length, native_function(f));
        self.define_on(
            &self.prototype,
            name,
            Self::value_descriptor(function.into(), attributes),
        )
    }

    /// A read-only accessor on the prototype whose getter finds its payload itself.
    pub fn prototype_getter(
        &mut self,
        name: &'static str,
        attributes: Attributes,
        getter: JSHostFn,
    ) -> JsResult<()> {
        let descriptor = PropertyDescriptor::builder()
            .get(self.function(&format!("get {name}"), 0, native_function(getter)))
            .enumerable(attributes.enumerable)
            .configurable(attributes.configurable)
            .build();
        self.define_on(&self.prototype, name, descriptor)
    }

    /// A method on the prototype keyed by the private name `symbol`, which a builtin reads as `this.$<symbol>`: on
    /// Boa the property `__intrinsic__<symbol>`, read-only and not enumerable. `name` is the function's.
    pub fn private_method(
        &mut self,
        symbol: &str,
        name: &'static str,
        length: usize,
        method: Method<T>,
    ) -> JsResult<()> {
        let function = self.function(
            name,
            length,
            NativeFunction::from_copy_closure(move |this, args, context| {
                call_host_fn(
                    |global, frame| method(&this_of::<T>(global, frame)?, global, frame),
                    this,
                    args,
                    context,
                )
            }),
        );
        let private = Attributes {
            writable: false,
            enumerable: false,
            configurable: true,
        };
        self.define_on(
            &self.prototype,
            &format!("__intrinsic__{symbol}"),
            Self::value_descriptor(function.into(), private),
        )
    }

    /// A method on the prototype written in JavaScript: `source` is a function in strict mode, which finds a
    /// private name of `this` as `this.__intrinsic__<name>`.
    pub fn builtin(
        &mut self,
        name: &str,
        length: usize,
        attributes: Attributes,
        source: &str,
    ) -> JsResult<()> {
        let function = self
            .global
            .evaluate_script(&format!("(function () {{ \"use strict\"; return ({source}); }})()"))?;
        let Some(object) = function.0.as_object().filter(|object| object.is_callable()) else {
            return Err(self.global.throw_type_error(format_args!(
                "{}.{name}: the builtin is not a function",
                T::NAME
            )));
        };
        let length_descriptor = PropertyDescriptor::builder()
            .value(length)
            .writable(false)
            .enumerable(false)
            .configurable(true)
            .build();
        self.define_on(&object, "length", length_descriptor)?;
        self.define_on(
            &self.prototype,
            name,
            Self::value_descriptor(function.0, attributes),
        )
    }

    fn getter_function(&self, name: &'static str, getter: Getter<T>) -> JsFunction {
        self.function(
            &format!("get {name}"),
            0,
            NativeFunction::from_copy_closure(move |this, args, context| {
                call_host_fn(
                    |global, frame| {
                        getter(
                            &accessor_this_of::<T>(global, frame, &format!("{name} getter"))?,
                            global,
                        )
                    },
                    this,
                    args,
                    context,
                )
            }),
        )
    }

    /// A read-only accessor on the prototype.
    pub fn getter(
        &mut self,
        name: &'static str,
        attributes: Attributes,
        getter: Getter<T>,
    ) -> JsResult<()> {
        let descriptor = PropertyDescriptor::builder()
            .get(self.getter_function(name, getter))
            .enumerable(attributes.enumerable)
            .configurable(attributes.configurable)
            .build();
        self.define_on(&self.prototype, name, descriptor)
    }

    /// An accessor on the prototype with a setter.
    pub fn accessor(
        &mut self,
        name: &'static str,
        attributes: Attributes,
        getter: Getter<T>,
        setter: Setter<T>,
    ) -> JsResult<()> {
        let set = self.function(
            &format!("set {name}"),
            1,
            NativeFunction::from_copy_closure(move |this, args, context| {
                call_host_fn(
                    |global, frame| {
                        let this = accessor_this_of::<T>(global, frame, &format!("{name} setter"))?;
                        setter(&this, global, frame.argument(0))?;
                        Ok(JSValue::UNDEFINED)
                    },
                    this,
                    args,
                    context,
                )
            }),
        );
        let descriptor = PropertyDescriptor::builder()
            .get(self.getter_function(name, getter))
            .set(set)
            .enumerable(attributes.enumerable)
            .configurable(attributes.configurable)
            .build();
        self.define_on(&self.prototype, name, descriptor)
    }

    /// A function on the constructor.
    pub fn static_method(
        &mut self,
        name: &str,
        length: usize,
        attributes: Attributes,
        f: JSHostFn,
    ) -> JsResult<()> {
        let function = self.function(name, length, native_function(f));
        self.define_on(
            &self.constructor,
            name,
            Self::value_descriptor(function.into(), attributes),
        )
    }

    /// A read-only value on the constructor.
    pub fn static_value(&mut self, name: &str, value: JSValue) -> JsResult<()> {
        let read_only = Attributes {
            writable: false,
            enumerable: false,
            configurable: false,
        };
        self.define_on(
            &self.constructor,
            name,
            Self::value_descriptor(value.0, read_only),
        )
    }
}

fn construct<T: JsClass>(
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> boa_engine::JsResult<JsValue> {
    call_host_fn(
        |global, frame| {
            let Some(new_target) = frame.this().get_object() else {
                return Err(ErrorCode::ILLEGAL_CONSTRUCTOR.throw(
                    global,
                    format_args!("{} constructor cannot be invoked without 'new'", T::NAME),
                ));
            };
            // `new.target.prototype`, for a subclass; the class's own otherwise. Read before the arguments are
            // converted, as JavaScriptCore's `structureForNewTarget` is.
            let prototype = match new_target.get(global, "prototype")?.get_object() {
                Some(prototype) => prototype.0,
                None => prototype_of::<T>(global)?,
            };
            let payload = T::construct(global, frame)?;
            let object = instance(prototype, payload);
            if let Some(this) = object.as_class_ref::<T>() {
                T::initialize(&this, global, frame)?;
            }
            Ok(object)
        },
        new_target,
        args,
        context,
    )
}

fn instance<T: JsClass>(prototype: JsObject, payload: T) -> JSValue {
    JSValue(
        JsObject::from_proto_and_data(prototype, Payload(Gc::new(GcRefCell::new(payload)))).into(),
    )
}

/// The class's constructor, which the caller puts where JavaScript finds it. The class is defined in this realm
/// the first time.
pub fn constructor<T: JsClass>(global: &JSGlobalObject) -> JsResult<JSValue> {
    let prototype = prototype_of::<T>(global)?;
    crate::JSObject(prototype).get(global, "constructor")
}

/// A JavaScript object whose payload is `value`.
pub fn to_js<T: JsClass>(global: &JSGlobalObject, value: T) -> JsResult<JSValue> {
    Ok(instance(prototype_of::<T>(global)?, value))
}

fn prototype_of<T: JsClass>(global: &JSGlobalObject) -> JsResult<JsObject> {
    if let Some(prototype) = global.state().prototypes.get(&TypeId::of::<T>()) {
        return Ok(prototype.clone());
    }
    let prototype = crate::JSObject::create(global).0;
    let constructor: JsObject = FunctionObjectBuilder::new(
        global.cx().realm(),
        NativeFunction::from_copy_closure(construct::<T>),
    )
    .name(JsString::from(T::NAME))
    .length(T::LENGTH)
    .constructor(true)
    .build()
    .into();
    let mut class = ClassBuilder::<T> {
        global,
        prototype: prototype.clone(),
        constructor: constructor.clone(),
        _payload: PhantomData,
    };
    let fixed = Attributes {
        writable: false,
        enumerable: false,
        configurable: false,
    };
    let hidden = Attributes {
        writable: true,
        enumerable: false,
        configurable: true,
    };
    class.define_on(
        &constructor,
        "prototype",
        ClassBuilder::<T>::value_descriptor(prototype.clone().into(), fixed),
    )?;
    class.define_on(
        &prototype,
        "constructor",
        ClassBuilder::<T>::value_descriptor(constructor.clone().into(), hidden),
    )?;
    // `Object.prototype.toString` says `[object T]`, as JavaScriptCore's `putToStringTagWithoutTransition`.
    let to_string_tag = PropertyDescriptor::builder()
        .value(JsString::from(T::NAME))
        .writable(false)
        .enumerable(false)
        .configurable(true)
        .build();
    let result =
        prototype.define_property_or_throw(JsSymbol::to_string_tag(), to_string_tag, global.cx());
    global.lift(result)?;
    // Before `define`, whose members may make instances.
    global
        .state()
        .prototypes
        .insert(TypeId::of::<T>(), prototype.clone());
    if let Err(error) = T::define(&mut class) {
        global.state().prototypes.remove(&TypeId::of::<T>());
        return Err(error);
    }
    Ok(prototype)
}
