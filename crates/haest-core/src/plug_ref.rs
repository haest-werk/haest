//! [`plug_ref!`](crate::plug_ref): the handle every plug point is held by.

/// Declares `<Trait>Ref`, the handle haest keeps a plug point's implementation
/// in: `pub struct ToolRef(Arc<dyn Tool>)`, so no signature names `Arc` or
/// `dyn`.
///
/// What it generates:
/// - `#[derive(Clone)]`: cheap, the clones share one object;
/// - `new(impl Trait + 'static)` and `ptr_eq(&a, &b)`;
/// - `From<T: Trait>`, so a parameter that takes `impl Into<XRef>` takes the
///   value itself;
/// - `From<Arc<T>>` and `From<Arc<dyn Trait>>`, so one object the caller
///   shares can fill several slots without a second `Arc`;
/// - `From<Box<dyn Trait>>`, for a part the host picks at run time;
/// - `Deref<Target = dyn Trait>` and `AsRef<dyn Trait>`, so the handle is called
///   like the trait;
/// - `Debug`: `XRef(..)`, or the `debug(this, f) { .. }` body given after the
///   struct (`this` is the `&dyn Trait`).
///
/// It never implements the trait for the handle: that would overlap the
/// reflexive `From<XRef>`. So a function that only borrows an implementation
/// for the call takes `&dyn Trait`, which a `&value` and a `&handle` both
/// coerce to, where `&impl Trait` would refuse the handle: haest's one place
/// for `dyn` in a signature (`&dyn Fs`, the net guard's `&dyn Resolve`).
///
/// ```ignore
/// haest_core::plug_ref! {
///     /// A [`Tool`], shared.
///     pub struct ToolRef(dyn Tool);
///     debug(tool, f) { f.debug_tuple("ToolRef").field(&tool.name()).finish() }
/// }
/// ```
#[macro_export]
#[doc(hidden)]
macro_rules! plug_ref {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident(dyn $trait:ident);
        debug($this:ident, $f:ident) $debug:block
    ) => {
        $crate::plug_ref!(@type $(#[$meta])* $vis $name $trait);

        impl ::std::fmt::Debug for $name {
            fn fmt(&self, $f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                let $this: &dyn $trait = &*self.0;
                $debug
            }
        }
    };
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident(dyn $trait:ident);
    ) => {
        $crate::plug_ref!(@type $(#[$meta])* $vis $name $trait);

        impl ::std::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.debug_tuple(stringify!($name)).finish_non_exhaustive()
            }
        }
    };
    (@type $(#[$meta:meta])* $vis:vis $name:ident $trait:ident) => {
        $(#[$meta])*
        #[derive(Clone)]
        $vis struct $name(::std::sync::Arc<dyn $trait>);

        impl $name {
            #[doc = concat!(
                "A handle to `value`. [`From`] does the same, so a parameter that takes `impl Into<",
                stringify!($name),
                ">` takes the value itself."
            )]
            pub fn new(value: impl $trait + 'static) -> Self {
                Self(::std::sync::Arc::new(value))
            }

            /// Whether both handles hold the same object (one is a clone of the other).
            pub fn ptr_eq(this: &Self, other: &Self) -> bool {
                ::std::sync::Arc::ptr_eq(&this.0, &other.0)
            }
        }

        impl<T: $trait + 'static> ::std::convert::From<T> for $name {
            fn from(value: T) -> Self {
                Self(::std::sync::Arc::new(value))
            }
        }

        impl<T: $trait + 'static> ::std::convert::From<::std::sync::Arc<T>> for $name {
            fn from(value: ::std::sync::Arc<T>) -> Self {
                Self(value)
            }
        }

        impl ::std::convert::From<::std::sync::Arc<dyn $trait>> for $name {
            fn from(value: ::std::sync::Arc<dyn $trait>) -> Self {
                Self(value)
            }
        }

        impl ::std::convert::From<::std::boxed::Box<dyn $trait>> for $name {
            fn from(value: ::std::boxed::Box<dyn $trait>) -> Self {
                Self(value.into())
            }
        }

        impl ::std::convert::AsRef<dyn $trait> for $name {
            fn as_ref(&self) -> &(dyn $trait + 'static) {
                &*self.0
            }
        }

        impl ::std::ops::Deref for $name {
            type Target = dyn $trait;

            fn deref(&self) -> &Self::Target {
                &*self.0
            }
        }
    };
}
