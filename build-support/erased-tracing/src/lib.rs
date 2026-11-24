//! An observer API adapter, never a second implementation of product behavior.
//!
//! Only the closed, pinned Linux dependency graph uses this adapter. It has no
//! observer state, clocks, imports, dependencies, initialization or Drop hooks.
//! Release verification must prove that its inline identity methods leave no
//! calls/symbols/strings in the shipped ELF; source-level no-ops are not proof.
#![no_std]

#[cfg(feature = "attributes")]
pub use erased_tracing_attributes::instrument;

#[derive(Clone, Copy, Debug, Default)]
pub struct Span;

#[derive(Clone, Copy, Debug)]
pub struct Entered;

impl Span {
    #[inline(always)]
    pub const fn none() -> Self { Self }
    #[inline(always)]
    pub const fn current() -> Self { Self }
    #[inline(always)]
    pub const fn enter(&self) -> Entered { Entered }
    #[inline(always)]
    pub const fn entered(self) -> Entered { Entered }
    #[inline(always)]
    pub const fn or_current(self) -> Self { self }
    #[inline(always)]
    pub const fn is_disabled(&self) -> bool { true }
    #[inline(always)]
    pub fn in_scope<T>(&self, operation: impl FnOnce() -> T) -> T { operation() }
}

pub trait Instrument: Sized {
    #[inline(always)]
    fn instrument(self, _: Span) -> Self { self }
    #[inline(always)]
    fn in_current_span(self) -> Self { self }
}
impl<T: Sized> Instrument for T {}

#[macro_export]
macro_rules! trace { ($($arguments:tt)*) => { () }; }
#[macro_export]
macro_rules! debug { ($($arguments:tt)*) => { () }; }
#[macro_export]
macro_rules! info { ($($arguments:tt)*) => { () }; }
#[macro_export]
macro_rules! warn { ($($arguments:tt)*) => { () }; }
#[macro_export]
macro_rules! error { ($($arguments:tt)*) => { () }; }
#[macro_export]
macro_rules! event { ($($arguments:tt)*) => { () }; }
#[macro_export]
macro_rules! enabled { ($($arguments:tt)*) => { false }; }
#[macro_export]
macro_rules! span { ($($arguments:tt)*) => { $crate::Span }; }
#[macro_export]
macro_rules! trace_span { ($($arguments:tt)*) => { $crate::Span }; }
#[macro_export]
macro_rules! debug_span { ($($arguments:tt)*) => { $crate::Span }; }
#[macro_export]
macro_rules! info_span { ($($arguments:tt)*) => { $crate::Span }; }
#[macro_export]
macro_rules! warn_span { ($($arguments:tt)*) => { $crate::Span }; }
#[macro_export]
macro_rules! error_span { ($($arguments:tt)*) => { $crate::Span }; }
