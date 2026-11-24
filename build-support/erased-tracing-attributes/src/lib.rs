//! Keep the original product item, erasing only its tracing attribute.
extern crate proc_macro;

#[proc_macro_attribute]
pub fn instrument(_: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    item
}
