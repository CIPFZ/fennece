//! `#[derive(Settings)]`: generates `Default`, setting descriptors and key-based
//! get/set for a flat settings struct.
//!
//! ```ignore
//! #[derive(Settings)]
//! pub struct Settings {
//!     /// Delay before searching, in milliseconds.
//!     #[setting(key = "search_delay", default = 0, page = "search")]
//!     pub search_delay: u32,
//! }
//! ```
//!
//! Every field must carry `#[setting(key = "...", default = <expr>, page = "...")]`.
//! `default` may be any expression of the field type; a string literal on a `String`
//! field is converted automatically. The field's doc comment becomes the descriptor's
//! description. Generated code refers to `::fnc_core::settings`.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Expr, ExprLit, Fields, Lit, LitStr, Meta, Type, parse_macro_input};

/// Derives `Default` and `fnc_core::settings::SettingsMeta`.
#[proc_macro_derive(Settings, attributes(setting))]
pub fn derive_settings(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

struct FieldSpec {
    ident: syn::Ident,
    ty: Type,
    key: LitStr,
    page: LitStr,
    default: Expr,
    doc: String,
}

fn is_string_type(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "String"))
}

fn parse_field(f: &syn::Field) -> syn::Result<FieldSpec> {
    let ident = f.ident.clone().ok_or_else(|| syn::Error::new_spanned(f, "Settings requires named fields"))?;
    let mut key = None;
    let mut page = None;
    let mut default = None;
    let mut doc = Vec::new();
    for attr in &f.attrs {
        if attr.path().is_ident("doc") {
            if let Meta::NameValue(nv) = &attr.meta
                && let Expr::Lit(ExprLit { lit: Lit::Str(s), .. }) = &nv.value
            {
                doc.push(s.value().trim().to_string());
            }
            continue;
        }
        if !attr.path().is_ident("setting") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("key") {
                key = Some(meta.value()?.parse::<LitStr>()?);
            } else if meta.path.is_ident("page") {
                page = Some(meta.value()?.parse::<LitStr>()?);
            } else if meta.path.is_ident("default") {
                default = Some(meta.value()?.parse::<Expr>()?);
            } else {
                return Err(meta.error("unknown setting attribute (expected key/default/page)"));
            }
            Ok(())
        })?;
    }
    let missing = |what: &str| syn::Error::new_spanned(&ident, format!("missing `{what}` in #[setting(...)]"));
    Ok(FieldSpec {
        key: key.ok_or_else(|| missing("key"))?,
        page: page.ok_or_else(|| missing("page"))?,
        default: default.ok_or_else(|| missing("default"))?,
        ty: f.ty.clone(),
        ident,
        doc: doc.join(" "),
    })
}

fn expand(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = &input.ident;
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(input, "Settings can only be derived for structs"));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(input, "Settings requires named fields"));
    };
    let specs = fields.named.iter().map(parse_field).collect::<syn::Result<Vec<_>>>()?;

    let mut seen = std::collections::HashSet::new();
    for s in &specs {
        if !seen.insert(s.key.value()) {
            return Err(syn::Error::new_spanned(&s.key, "duplicate setting key"));
        }
    }

    let defaults = specs.iter().map(|s| {
        let ident = &s.ident;
        let ty = &s.ty;
        let expr = &s.default;
        let value = match expr {
            Expr::Lit(ExprLit { lit: Lit::Str(lit), .. }) if is_string_type(ty) => {
                quote! { ::std::string::String::from(#lit) }
            }
            _ => quote! { #expr },
        };
        quote! { #ident: { let v: #ty = #value; v } }
    });

    let descriptors = specs.iter().map(|s| {
        let key = &s.key;
        let page = &s.page;
        let ty = &s.ty;
        let field = s.ident.to_string();
        let doc = &s.doc;
        let expr = &s.default;
        let default_src = quote!(#expr).to_string();
        quote! {
            ::fnc_core::settings::SettingDescriptor {
                key: #key,
                page: #page,
                field: #field,
                doc: #doc,
                default_src: #default_src,
                kind: <#ty as ::fnc_core::settings::SettingType>::KIND,
            }
        }
    });

    let get_arms = specs.iter().map(|s| {
        let key = &s.key;
        let ident = &s.ident;
        quote! { #key => ::std::option::Option::Some(::fnc_core::settings::SettingType::to_setting_string(&self.#ident)), }
    });
    let set_arms = specs.iter().map(|s| {
        let key = &s.key;
        let ident = &s.ident;
        let ty = &s.ty;
        quote! {
            #key => {
                self.#ident = <#ty as ::fnc_core::settings::SettingType>::parse_setting(value)
                    .map_err(|message| ::fnc_core::settings::SettingError::Invalid { key: #key, message })?;
                ::std::result::Result::Ok(())
            }
        }
    });
    let reset_arms = specs.iter().map(|s| {
        let key = &s.key;
        let ident = &s.ident;
        quote! { #key => { self.#ident = d.#ident; true } }
    });
    let count = specs.len();

    Ok(quote! {
        impl ::std::default::Default for #name {
            fn default() -> Self {
                Self { #(#defaults,)* }
            }
        }

        impl ::fnc_core::settings::SettingsMeta for #name {
            fn descriptors() -> &'static [::fnc_core::settings::SettingDescriptor] {
                static DESCRIPTORS: [::fnc_core::settings::SettingDescriptor; #count] = [#(#descriptors),*];
                &DESCRIPTORS
            }

            fn get(&self, key: &str) -> ::std::option::Option<::std::string::String> {
                match key {
                    #(#get_arms)*
                    _ => ::std::option::Option::None,
                }
            }

            fn set(&mut self, key: &str, value: &str) -> ::std::result::Result<(), ::fnc_core::settings::SettingError> {
                match key {
                    #(#set_arms)*
                    _ => ::std::result::Result::Err(::fnc_core::settings::SettingError::UnknownKey(key.to_string())),
                }
            }

            fn reset(&mut self, key: &str) -> bool {
                let d = <Self as ::std::default::Default>::default();
                match key {
                    #(#reset_arms)*
                    _ => false,
                }
            }
        }
    })
}
