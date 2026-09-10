//! Upstream IDL generation, adapted to Aileron's owned async messages.
//!
//! zlink-codegen 0.7 emits borrowed types and unary proxies without introspection
//! derives. Adjust that syntax tree here; never duplicate the IDL's fields/types.
use std::{collections::HashMap, env, fs, path::PathBuf};

use quote::{format_ident, quote};
use syn::{
    Item, TraitItem, Type, parse_quote,
    visit_mut::{self, VisitMut},
};

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for name in ["Inference", "Models", "Permissions", "Sessions"] {
        let path = format!("varlink/aileron.{name}.varlink");
        println!("cargo:rerun-if-changed={path}");
        let source = fs::read_to_string(path).unwrap();
        let idl = zlink::idl::Interface::try_from(source.as_str()).unwrap();
        let generated = zlink_codegen::generate_interface(&idl).unwrap();
        let mut file = syn::parse_file(&generated).expect("zlink-codegen must emit Rust");
        file.attrs.clear(); // include! cannot contain inner module documentation.

        let mut names = HashMap::from([
            (name.to_string(), "VarlinkClientInterface".to_string()),
            (format!("{name}Error"), "Error".to_string()),
        ]);
        for method in idl.methods() {
            names.insert(
                format!("{}Output", method.name()),
                format!("{}_Reply", method.name()),
            );
        }
        Owned { names }.visit_file_mut(&mut file);

        let custom_types = idl
            .custom_types()
            .map(|ty| format_ident!("{}", ty.name()))
            .collect::<Vec<_>>();
        let mut streams = Vec::new();
        for item in &mut file.items {
            match item {
                Item::Struct(item) => {
                    let derive = if custom_types.contains(&item.ident) {
                        parse_quote!(#[derive(zlink::introspect::CustomType)])
                    } else {
                        parse_quote!(#[derive(zlink::introspect::Type)])
                    };
                    item.attrs.push(derive);
                }
                Item::Enum(item) => item
                    .attrs
                    .push(parse_quote!(#[derive(zlink::introspect::ReplyError)])),
                Item::Trait(proxy) => {
                    // Streaming is a call flag, not expressible in Varlink IDL.
                    // Inference Stream* methods use our connection-owning cursor;
                    // install methods use zlink's native LocalSet-compatible proxy.
                    for (item, method) in proxy.items.iter_mut().zip(idl.methods()) {
                        let TraitItem::Fn(function) = item else {
                            unreachable!()
                        };
                        let reply = format_ident!("{}_Reply", method.name());
                        if name == "Inference" && method.name().starts_with("Stream") {
                            let mut function = function.clone();
                            *function.sig.inputs.first_mut().unwrap() = parse_quote!(self);
                            function.sig.output = parse_quote!(-> zlink::Result<crate::stream::InferenceReplyStream<#reply>>);
                            let args = function
                                .sig
                                .inputs
                                .iter()
                                .skip(1)
                                .map(|arg| {
                                    let syn::FnArg::Typed(arg) = arg else {
                                        unreachable!()
                                    };
                                    let syn::Pat::Ident(arg) = arg.pat.as_ref() else {
                                        unreachable!()
                                    };
                                    &arg.ident
                                })
                                .collect::<Vec<_>>();
                            let keys = method
                                .inputs()
                                .map(|field| field.name())
                                .collect::<Vec<_>>();
                            let wire_name = format!("{}.{}", idl.name(), method.name());
                            function.default = Some(parse_quote!({
                                crate::stream::start_stream(self.into(), #wire_name,
                                    serde_json::json!({ #(#keys: #args),* })
                                ).await
                            }));
                            function.semi_token = None;
                            streams.push(function);
                        } else if name == "Models"
                            && matches!(method.name(), "InstallManifest" | "InstallUrlProfile")
                        {
                            function.attrs.push(parse_quote!(#[zlink(more)]));
                            function.sig.output = parse_quote!(-> zlink::Result<impl zlink::futures_util::Stream<Item = zlink::Result<Result<#reply, Error>>>>);
                        }
                    }
                    proxy.items.retain(|item| !matches!(item, TraitItem::Fn(function) if streams.iter().any(|stream| stream.sig.ident == function.sig.ident)));
                }
                _ => {}
            }
        }
        let extras = quote! {
            pub type Result<T, E = Error> = std::result::Result<T, E>;
            pub const CUSTOM_TYPES: &[&zlink::idl::CustomType<'static>] = &[
                #(<#custom_types as zlink::introspect::CustomType>::CUSTOM_TYPE),*
            ];
        };
        file.items
            .extend(syn::parse2::<syn::File>(extras).unwrap().items);
        if !streams.is_empty() {
            file.items.extend(syn::parse2::<syn::File>(quote! {
                pub use crate::stream::InferenceReplyStream;
                #[allow(async_fn_in_trait)]
                pub trait VarlinkStreamingClientInterface: Into<zlink::tokio::unix::Connection> + Sized {
                    #(#streams)*
                }
                impl VarlinkStreamingClientInterface for zlink::tokio::unix::Connection {}
            }).unwrap().items);
        }
        fs::write(
            out.join(format!("aileron_{name}.rs")),
            prettyplease::unparse(&file),
        )
        .unwrap();
    }
}

struct Owned {
    names: HashMap<String, String>,
}

impl VisitMut for Owned {
    fn visit_receiver_mut(&mut self, _receiver: &mut syn::Receiver) {
        // Unary proxies still borrow their connection; only messages are owned.
    }

    fn visit_ident_mut(&mut self, ident: &mut syn::Ident) {
        if let Some(name) = self.names.get(&ident.to_string()) {
            *ident = format_ident!("{name}");
        }
    }

    fn visit_generics_mut(&mut self, generics: &mut syn::Generics) {
        generics.params = generics
            .params
            .clone()
            .into_iter()
            .filter(|param| !matches!(param, syn::GenericParam::Lifetime(_)))
            .collect();
        visit_mut::visit_generics_mut(self, generics);
    }

    fn visit_field_mut(&mut self, field: &mut syn::Field) {
        field.attrs.retain(|attr| {
            !(attr.path().is_ident("serde")
                && attr
                    .parse_args::<syn::Ident>()
                    .is_ok_and(|ident| ident == "borrow"))
        });
        visit_mut::visit_field_mut(self, field);
    }

    fn visit_type_mut(&mut self, ty: &mut Type) {
        if let Type::Reference(reference) = ty {
            *ty = match reference.elem.as_ref() {
                Type::Path(path) if path.path.is_ident("str") => parse_quote!(String),
                Type::Slice(slice) => {
                    let elem = &slice.elem;
                    parse_quote!(Vec<#elem>)
                }
                other => other.clone(),
            };
        }
        if let Type::Path(path) = ty {
            for segment in &mut path.path.segments {
                if let syn::PathArguments::AngleBracketed(args) = &mut segment.arguments {
                    args.args = args
                        .args
                        .clone()
                        .into_iter()
                        .filter(|arg| !matches!(arg, syn::GenericArgument::Lifetime(_)))
                        .collect();
                    if args.args.is_empty() {
                        segment.arguments = syn::PathArguments::None;
                    }
                }
            }
        }
        visit_mut::visit_type_mut(self, ty);
    }
}
