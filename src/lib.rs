extern crate self as applicationlauncher;

#[cfg(all(feature = "release-minimal", feature = "observability"))]
compile_error!("release-minimal requires --no-default-features; it cannot include observability");

#[cfg(feature = "observability")]
pub mod diagnostic_capture;
pub mod freshness;
pub mod lifecycle;
#[cfg(feature = "observability")]
pub mod observability;
pub mod operation;
pub mod process;
#[cfg(feature = "observability")]
pub mod replay;
pub mod tracker;

pub const BUILD_ID: &str = env!("APPLICATIONLAUNCHER_BUILD_ID");

pub const BUILD_INFO: &str = include_str!(concat!(
    env!("OUT_DIR"),
    "/lifecycle/artifact-manifest.json"
));

/// Observer arguments are tokens, not evaluated expressions, in the minimal artifact.
#[macro_export]
macro_rules! observe {
    ($category:literal => $($body:tt)*) => {{
        #[cfg(feature = "observability")]
        if $crate::observability::category_enabled($category) {
            $($body)*
        }
    }};
    ($($body:tt)*) => {{
        $crate::observe!("baseline" => $($body)*);
    }};
}

#[macro_export]
macro_rules! diagnostic_log {
    ($($arguments:tt)*) => {
        $crate::observe! { "stderr" => std::eprintln!($($arguments)*); }
    };
}

#[macro_export]
macro_rules! diagnostic_line {
    ($message:expr $(,)?) => {
        $crate::diagnostic_log!("{}", $message)
    };
}

/// Product threads remain; their registry, labels and worker-state calls do not.
#[macro_export]
macro_rules! spawn_worker {
    ($name:expr, move |$worker:ident| $body:block $(,)?) => {{
        #[cfg(feature = "observability")]
        {
            $crate::observability::spawn_named($name, move |$worker| $body)
        }
        #[cfg(not(feature = "observability"))]
        {
            std::thread::spawn(move || $body)
        }
    }};
    ($name:expr, |$worker:ident| $body:block $(,)?) => {
        $crate::spawn_worker!($name, move |$worker| $body)
    };
}

#[cfg(all(test, not(feature = "observability")))]
mod erasure_tests {
    #[test]
    fn erased_tokens_need_no_observer_imports_and_product_work_still_runs() {
        crate::observe! { undefined_observer_api::allocate_and_record(); }
        crate::diagnostic_log!("{}", undefined_observer_argument());
        let result = crate::spawn_worker!(undefined_observer_label(), move |worker| {
            crate::observe! { worker.set_state("observer-only"); }
            7
        });
        assert_eq!(result.join().unwrap(), 7);
    }
}

/// Copy a compile-time producer record into an immutable ELF byte-array section.
pub const fn metadata_bytes<const N: usize>(text: &str) -> [u8; N] {
    assert!(text.len() == N);
    let mut bytes = [0; N];
    let mut index = 0;
    while index < N {
        bytes[index] = text.as_bytes()[index];
        index += 1;
    }
    bytes
}
