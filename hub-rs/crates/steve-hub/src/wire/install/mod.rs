//! Startup-only registries: typed slots carry dependencies between ordered installers.

// Moving slots out keeps ownership (including Router state transitions) explicit. A missing slot
// is a programmer error in the installer order, never a lookup by service name or runtime type.
macro_rules! take {
    ($reg:ident;) => {};
    ($reg:ident; mut $field:ident $(, $($rest:tt)*)?) => {
        let mut $field = $reg.$field.take().expect(concat!(stringify!($field), " installed earlier"));
        take!($reg; $($($rest)*)?);
    };
    ($reg:ident; $field:ident $(, $($rest:tt)*)?) => {
        let $field = $reg.$field.take().expect(concat!(stringify!($field), " installed earlier"));
        take!($reg; $($($rest)*)?);
    };
}
macro_rules! put {
    ($reg:ident; $($field:ident),* $(,)?) => {
        $($reg.$field = Some($field);)*
    };
}

// A `mod` entry declares its file as well as adding an installer. Later phases can reuse it.
macro_rules! ordered_installers {
    (@collect $name:ident: $ty:ty; [$($decl:tt)*] [$($entry:expr,)*];) => {
        $($decl)*
        const $name: &[$ty] = &[$($entry,)*];
    };
    (@collect $name:ident: $ty:ty; [$($decl:tt)*] [$($entry:expr,)*]; mod $area:ident; $install:ident, $($tail:tt)*) => {
        ordered_installers!(@collect $name: $ty; [$($decl)* mod $area;] [$($entry,)* $area::$install,]; $($tail)*);
    };
    (@collect $name:ident: $ty:ty; [$($decl:tt)*] [$($entry:expr,)*]; $area:ident::$install:ident, $($tail:tt)*) => {
        ordered_installers!(@collect $name: $ty; [$($decl)*] [$($entry,)* $area::$install,]; $($tail)*);
    };
    ($name:ident: $ty:ty; $($entry:tt)*) => {
        ordered_installers!(@collect $name: $ty; [] []; $($entry)*);
    };
}

pub(super) mod domain;
pub(super) mod hub;
