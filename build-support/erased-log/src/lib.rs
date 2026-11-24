//! This adapter must disappear completely from the linked release postimage.
#![no_std]

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Level { Error, Warn, Info, Debug, Trace }
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum LevelFilter { Off, Error, Warn, Info, Debug, Trace }

pub const STATIC_MAX_LEVEL: LevelFilter = LevelFilter::Off;
#[inline(always)]
pub const fn max_level() -> LevelFilter { LevelFilter::Off }
#[inline(always)]
pub fn set_max_level(_: LevelFilter) {}

#[macro_export]
macro_rules! log { ($($arguments:tt)*) => { () }; }
#[macro_export]
macro_rules! error { ($($arguments:tt)*) => { () }; }
#[macro_export]
macro_rules! warn { ($($arguments:tt)*) => { () }; }
#[macro_export]
macro_rules! info { ($($arguments:tt)*) => { () }; }
#[macro_export]
macro_rules! debug { ($($arguments:tt)*) => { () }; }
#[macro_export]
macro_rules! trace { ($($arguments:tt)*) => { () }; }
#[macro_export]
macro_rules! log_enabled { ($($arguments:tt)*) => { false }; }
