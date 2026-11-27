//! `JsWrapper<'a, T>`, the JavaScript object of a payload, as `bun_jsc::bindgen` has it: what `passThis` hands a
//! member, what `b.self` returns, and what `JsPayload::into_js` makes of a new payload.

use super::{JsHeap, JsLocal, Result};
use crate::class::ClassRef;
use crate::{JSValue, JsClass};

/// An instance of `T`, for the call.
pub struct JsWrapper<'a, T: JsClass>(pub(crate) ClassRef<T>, super::Invariant<'a>);

impl<T: JsClass> Clone for JsWrapper<'_, T> {
    fn clone(&self) -> Self {
        JsWrapper(self.0.clone(), core::marker::PhantomData)
    }
}

impl<'a, T: JsClass> JsWrapper<'a, T> {
    #[doc(hidden)]
    pub fn of(this: &ClassRef<T>) -> Self {
        JsWrapper(this.clone(), core::marker::PhantomData)
    }

    pub fn class_ref(&self) -> &ClassRef<T> {
        &self.0
    }

    pub fn to_js(&self) -> JSValue {
        self.0.to_js()
    }

    pub fn as_value(&self) -> JsLocal<'a> {
        JsLocal::from_raw(self.0.to_js())
    }
}

/// A payload made into a new instance.
pub trait JsPayload: JsClass {
    fn into_js<'a>(self, heap: &JsHeap<'a>) -> JsWrapper<'a, Self> {
        let value = crate::class::to_js(heap.global_object(), self).expect("the class is defined in this realm");
        let this = value.as_class_ref::<Self>().expect("an instance of the class it was made of");
        JsWrapper(this, core::marker::PhantomData)
    }
}

impl<T: JsClass> JsPayload for T {}

/// What a body returns for a class: a new payload, or an instance it has.
pub trait IntoInstance<'a, T: JsClass> {
    fn into_instance(self, heap: &JsHeap<'a>) -> Result<'a, JSValue>;
}

impl<'a, T: JsClass> IntoInstance<'a, T> for T {
    fn into_instance(self, heap: &JsHeap<'a>) -> Result<'a, JSValue> {
        Ok(crate::class::to_js(heap.global_object(), self)?)
    }
}

impl<'a, T: JsClass> IntoInstance<'a, T> for JsWrapper<'a, T> {
    fn into_instance(self, _: &JsHeap<'a>) -> Result<'a, JSValue> {
        Ok(self.to_js())
    }
}

impl<'a, T: JsClass> super::to_js::Any<'a> for JsWrapper<'a, T> {
    fn into_value(self, _: &JsHeap<'a>) -> JsLocal<'a> {
        self.as_value()
    }
}
