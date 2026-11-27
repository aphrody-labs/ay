//! GC roots that Rust holds: `Strong` keeps a value alive, `Weak` does not.

use boa_engine::object::WeakJsObject;
use boa_gc::{Finalize, Trace};

use crate::{JSGlobalObject, JSValue};

/// A value that stays alive while Rust holds it. Boa's collector roots any value the heap does not account for,
/// so this is the value itself; it exists for the shape of `bun_jsc::Strong`. In a class payload it is traced,
/// as a `WriteBarrier` would be.
#[derive(Clone, Debug, Default, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
pub struct Strong(Option<JSValue>);

impl Strong {
    pub const fn empty() -> Strong {
        Strong(None)
    }

    pub fn create(value: JSValue, _: &JSGlobalObject) -> Strong {
        Strong(Some(value))
    }

    /// The value, `undefined` if there is none.
    pub fn get(&self) -> JSValue {
        self.0.clone().unwrap_or_default()
    }

    pub fn try_get(&self) -> Option<JSValue> {
        self.0.clone()
    }

    pub fn has(&self) -> bool {
        self.0.is_some()
    }

    pub fn set(&mut self, _: &JSGlobalObject, value: JSValue) {
        self.0 = Some(value);
    }

    /// The value, which the root no longer holds.
    pub fn swap(&mut self) -> JSValue {
        self.0.take().unwrap_or_default()
    }

    pub fn clear(&mut self) {
        self.0 = None;
    }
}

/// An object that Rust refers to without keeping it alive. A primitive has no lifetime to end: it is held as is.
#[derive(Clone, Default)]
pub struct Weak(Option<WeakRepr>);

#[derive(Clone)]
enum WeakRepr {
    Object(WeakJsObject),
    Primitive(JSValue),
}

impl Weak {
    pub fn create(value: JSValue, _: &JSGlobalObject) -> Weak {
        Weak(Some(match value.0.as_object() {
            Some(object) => WeakRepr::Object(object.downgrade()),
            None => WeakRepr::Primitive(value),
        }))
    }

    /// The value while it is alive.
    pub fn get(&self) -> Option<JSValue> {
        match self.0.as_ref()? {
            WeakRepr::Object(object) => object.upgrade().map(|object| JSValue(object.into())),
            WeakRepr::Primitive(value) => Some(value.clone()),
        }
    }

    pub fn clear(&mut self) {
        self.0 = None;
    }
}
