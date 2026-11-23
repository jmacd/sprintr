// SPDX-License-Identifier: Apache-2.0

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as Tokens};
use quote::{ToTokens, format_ident, quote};
use sprintr_message::format::{Part, Spec, parse_capture};
use sprintr_message::validate_relation_name;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    Expr, ExprLit, Ident, Lit, LitByteStr, LitStr, Meta, Path, Token, Type, Visibility,
    parse_macro_input,
};

struct Input {
    root: Path,
    values: Vec<Path>,
    format: LitStr,
    arguments: Vec<Expr>,
}

impl Parse for Input {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let root = input.parse()?;
        input.parse::<Token![;]>()?;
        let content;
        syn::bracketed!(content in input);
        let values = Punctuated::<Path, Token![,]>::parse_terminated(&content)?
            .into_iter()
            .collect();
        input.parse::<Token![;]>()?;
        let format = input.parse()?;
        let mut arguments = Vec::new();
        while !input.is_empty() {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            arguments.push(input.parse()?);
        }
        Ok(Self {
            root,
            values,
            format,
            arguments,
        })
    }
}

fn bytes(text: &str, span: Span) -> LitByteStr {
    LitByteStr::new(text.as_bytes(), span)
}

fn json(text: &str) -> String {
    serde_json::to_string(text).expect("string JSON encoding")
}

fn json_content(text: &str, span: Span) -> LitByteStr {
    let encoded = json(text);
    bytes(&encoded[1..encoded.len() - 1], span)
}

/// Doc comments, `#[anonymous]`, and the description derived from the docs.
struct Documented {
    docs: Vec<syn::Attribute>,
    description: String,
    anonymous: bool,
}

fn documented(attrs: Vec<syn::Attribute>, ident: &Ident) -> syn::Result<Documented> {
    let mut docs = Vec::new();
    let mut lines = Vec::new();
    let mut anonymous = false;
    for attr in attrs {
        if attr.path().is_ident("anonymous") {
            attr.meta.require_path_only()?;
            anonymous = true;
        } else if attr.path().is_ident("doc") {
            if let Meta::NameValue(meta) = &attr.meta
                && let Expr::Lit(ExprLit {
                    lit: Lit::Str(text),
                    ..
                }) = &meta.value
            {
                lines.push(text.value().trim().to_string());
            }
            docs.push(attr);
        } else {
            return Err(syn::Error::new_spanned(
                attr,
                "only doc comments and #[anonymous] are supported here",
            ));
        }
    }
    let description = lines.join("\n").trim().to_string();
    if description.is_empty() {
        return Err(syn::Error::new(
            ident.span(),
            "add a doc comment; it becomes the recorded description",
        ));
    }
    Ok(Documented {
        docs,
        description,
        anonymous,
    })
}

fn relation_name(name: &LitStr) -> syn::Result<String> {
    let value = name.value();
    validate_relation_name(&value).map_err(|error| syn::Error::new(name.span(), error))?;
    Ok(value)
}

fn register(root: &Path, declaration: Tokens) -> Tokens {
    quote! {
        const _: () = {
            #[#root::__private::linkme::distributed_slice(#root::__private::DECLARATIONS)]
            #[linkme(crate = #root::__private::linkme)]
            static DECLARATION: #root::__private::Declaration = #declaration;
        };
    }
}

struct AttributeItem {
    attrs: Vec<syn::Attribute>,
    vis: Visibility,
    ident: Ident,
    ty: AttributeType,
    name: LitStr,
}

enum AttributeType {
    Value(Type),
    Entity(Path),
}

struct Declarations<T> {
    root: Path,
    items: Vec<T>,
}

impl<T: Parse> Parse for Declarations<T> {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let root = input.parse()?;
        input.parse::<Token![;]>()?;
        let mut items = Vec::new();
        while !input.is_empty() {
            items.push(input.parse()?);
        }
        Ok(Self { root, items })
    }
}

impl Parse for AttributeItem {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let attrs = input.call(syn::Attribute::parse_outer)?;
        let vis = input.parse()?;
        let ident = input.parse()?;
        input.parse::<Token![:]>()?;
        let fork = input.fork();
        let ty = if fork.parse::<Ident>().is_ok_and(|ident| ident == "entity")
            && fork.peek(syn::token::Paren)
        {
            input.parse::<Ident>()?;
            let target;
            syn::parenthesized!(target in input);
            let path = target.parse()?;
            if !target.is_empty() {
                return Err(target.error("expected a single entity relation path"));
            }
            AttributeType::Entity(path)
        } else {
            AttributeType::Value(input.parse()?)
        };
        input.parse::<Token![=]>()?;
        let name = input.parse()?;
        input.parse::<Token![;]>()?;
        Ok(Self {
            attrs,
            vis,
            ident,
            ty,
            name,
        })
    }
}

/// Declare attributes whose doc comments become their recorded descriptions.
#[proc_macro]
pub fn attributes(input: TokenStream) -> TokenStream {
    let Declarations { root, items } = parse_macro_input!(input as Declarations<AttributeItem>);
    items
        .into_iter()
        .map(|item| attribute(&root, item))
        .collect::<syn::Result<Tokens>>()
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn attribute(root: &Path, item: AttributeItem) -> syn::Result<Tokens> {
    let AttributeItem {
        attrs,
        vis,
        ident,
        ty,
        name,
    } = item;
    let Documented {
        docs,
        description,
        anonymous,
    } = documented(attrs, &ident)?;
    if anonymous {
        return Err(syn::Error::new(
            ident.span(),
            "attributes cannot be anonymous",
        ));
    }
    let value = relation_name(&name)?;
    let (ty, meta) = match ty {
        AttributeType::Entity(target) => (
            quote!(#root::EntityHandle),
            quote!(#root::__private::RelationMeta::__entity_attribute(#name, #description, &#target)),
        ),
        AttributeType::Value(ty) => {
            let value_type = match ty.to_token_stream().to_string().as_str() {
                "str" => quote!(#root::RelationType::String),
                "u64" => quote!(#root::RelationType::U64),
                "[u64]" => quote!(#root::RelationType::U64Array),
                _ => {
                    return Err(syn::Error::new_spanned(
                        ty,
                        "attribute types are `str`, `u64`, `[u64]`, or `entity(relation)`",
                    ));
                }
            };
            (
                quote!(#ty),
                quote!(#root::__private::RelationMeta::__new(
                    #name, #root::RelationKind::Attribute, #value_type, false, #description,
                )),
            )
        }
    };
    let label = json_content(&format!("@{value}"), name.span());
    let reference = bytes(&format!("{{{}:{{\"$arg\":", json(&value)), name.span());
    let registration = register(
        root,
        quote!(#root::__private::Declaration::Relation(#ident.__meta())),
    );
    Ok(quote! {
        #(#docs)*
        #[allow(non_upper_case_globals)]
        #vis static #ident: #root::Attribute<#ty> = #root::Attribute::__new(
            #meta,
            #label,
            #reference,
        );
        #registration
    })
}

struct EntityItem {
    attrs: Vec<syn::Attribute>,
    vis: Visibility,
    ident: Ident,
    name: LitStr,
    values: Option<Vec<(Vec<syn::Attribute>, Ident)>>,
}

impl Parse for EntityItem {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let attrs = input.call(syn::Attribute::parse_outer)?;
        let vis = input.parse()?;
        let ident = input.parse()?;
        input.parse::<Token![=]>()?;
        let name = input.parse()?;
        let values = if input.peek(syn::token::Brace) {
            let content;
            syn::braced!(content in input);
            let mut values = Vec::new();
            while !content.is_empty() {
                let attrs = content.call(syn::Attribute::parse_outer)?;
                values.push((attrs, content.parse()?));
                if content.is_empty() {
                    break;
                }
                content.parse::<Token![,]>()?;
            }
            if input.peek(Token![;]) {
                input.parse::<Token![;]>()?;
            }
            Some(values)
        } else {
            input.parse::<Token![;]>()?;
            None
        };
        Ok(Self {
            attrs,
            vis,
            ident,
            name,
            values,
        })
    }
}

/// Declare entity relations and any statically named entities.
#[proc_macro]
pub fn entities(input: TokenStream) -> TokenStream {
    let Declarations { root, items } = parse_macro_input!(input as Declarations<EntityItem>);
    items
        .into_iter()
        .map(|item| entity(&root, item))
        .collect::<syn::Result<Tokens>>()
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn entity(root: &Path, item: EntityItem) -> syn::Result<Tokens> {
    let EntityItem {
        attrs,
        vis,
        ident,
        name,
        values,
    } = item;
    let Documented {
        docs,
        description,
        anonymous,
    } = documented(attrs, &ident)?;
    let relation = relation_name(&name)?;
    let registration = register(
        root,
        quote!(#root::__private::Declaration::Relation(#ident.__meta())),
    );
    let relation_static = |vis: Tokens, ident: &Ident| {
        quote! {
            #[allow(non_upper_case_globals)]
            #vis static #ident: #root::Entity<#anonymous> = #root::Entity::__new(
                #root::__private::RelationMeta::__new(
                    #name,
                    #root::RelationKind::Entity,
                    #root::RelationType::Object,
                    #anonymous,
                    #description,
                ),
            );
        }
    };
    let Some(values) = values else {
        let relation = relation_static(quote!(#vis), &ident);
        return Ok(quote! {
            #(#docs)*
            #relation
            #registration
        });
    };
    {
        if anonymous {
            return Err(syn::Error::new(
                ident.span(),
                "anonymous entities cannot have named values",
            ));
        }
        let mut named = Vec::new();
        for (attrs, value) in values {
            let Documented {
                docs,
                description,
                anonymous,
            } = documented(attrs, &value)?;
            if anonymous {
                return Err(syn::Error::new(
                    value.span(),
                    "named values cannot be anonymous",
                ));
            }
            let value_name = value.to_string();
            let fragment = bytes(
                &format!("{}:{}", json(&relation), json(&value_name)),
                value.span(),
            );
            let registration =
                register(root, quote!(#root::__private::Declaration::Named(&#value)));
            named.push(quote! {
                #(#docs)*
                #[allow(non_upper_case_globals)]
                pub static #value: #root::Named = #root::Named::__new(
                    &__RELATION,
                    #value_name,
                    #description,
                    #fragment,
                );
                #registration
            });
        }
        // The relation lives in the module of its values, so that they need no
        // `super::` path, and is re-exported under the same name: modules and
        // statics are in different namespaces. This works inside functions.
        let relation = relation_static(quote!(pub), &format_ident!("__RELATION"));
        Ok(quote! {
            #(#docs)*
            #[allow(non_snake_case)]
            #vis mod #ident {
                #[doc(hidden)]
                #relation
                #(#named)*
            }
            #(#docs)*
            #vis use #ident::__RELATION as #ident;
            #registration
        })
    }
}

struct Compiled {
    declaration: Tokens,
    stack: Tokens,
    arguments: Vec<Tokens>,
}

fn compile(input: &Input, nested: bool) -> syn::Result<Compiled> {
    let root = &input.root;
    let span = input.format.span();
    let error = |message: &str| syn::Error::new(span, message);
    let parts = parse_capture(&input.format.value()).map_err(|err| error(&err.to_string()))?;
    let mut positional = input.arguments.iter();
    let mut arguments = Vec::new();
    let mut attributes = Vec::new();
    let mut message = Vec::new();
    let mut references = Vec::new();
    for part in parts {
        match part {
            Part::Literal(text) => {
                let literal = json_content(&text.replace('{', "{{").replace('}', "}}"), span);
                message.push(quote!(#literal));
            }
            Part::Field(field) => {
                let (label, expression, attribute) = if field.name.is_empty() {
                    let expression = positional
                        .next()
                        .ok_or_else(|| error("missing positional argument"))?;
                    if let Expr::Assign(assignment) = expression {
                        let Expr::Path(attribute) = assignment.left.as_ref() else {
                            return Err(syn::Error::new_spanned(
                                &assignment.left,
                                "expected a declared attribute path",
                            ));
                        };
                        let value = &assignment.right;
                        (String::new(), quote!(#value), Some(quote!(#attribute)))
                    } else {
                        let label = expression.to_token_stream().to_string();
                        if label.contains(['{', '}', ':']) {
                            return Err(syn::Error::new_spanned(
                                expression,
                                "bind this expression to a local variable before logging it",
                            ));
                        }
                        (label, quote!(#expression), None)
                    }
                } else {
                    let mut ident = syn::parse_str::<Ident>(&field.name)
                        .map_err(|_| error("captured placeholders must name a local variable"))?;
                    ident.set_span(span);
                    (field.name, quote!(#ident), None)
                };
                let suffix = if field.spec == Spec::String {
                    String::new()
                } else {
                    format!(":{}", field.format)
                };
                if let Some(attribute) = attribute {
                    if field.format == "?" {
                        return Err(error("use :s or :x for a typed attribute, not Debug text"));
                    }
                    let position = arguments.len();
                    if nested {
                        attributes.push(quote!((#position, (#attribute).__meta())));
                    } else {
                        if !attributes.is_empty() {
                            references.push(quote!(b","));
                        }
                        let index = bytes(&format!("{position}}}}}"), span);
                        references.push(quote!((#attribute).__reference()));
                        references.push(quote!(#index));
                        attributes.push(quote!((#attribute).__meta()));
                    }
                    let suffix = bytes(&format!("{suffix}}}"), span);
                    message.push(quote!(b"{"));
                    message.push(quote!((#attribute).__label()));
                    message.push(quote!(#suffix));
                    // The method's existence enforces the attribute's declared type.
                    arguments.push(match (field.spec, field.format.is_empty()) {
                        (Spec::String, true) => quote!((#attribute).__value(&(#expression))),
                        (Spec::String, false) => {
                            quote!((#attribute).__string_value(&(#expression)))
                        }
                        (Spec::Hex { .. }, _) => quote!((#attribute).__hex_value(&(#expression))),
                    });
                } else {
                    let label = json_content(&format!("{{{label}{suffix}}}"), span);
                    message.push(quote!(#label));
                    arguments.push(match (field.spec, field.format.is_empty()) {
                        (Spec::String, true) => {
                            let scalars =
                                nested.then(|| quote!(use #root::__private::ScalarArgument as _;));
                            quote!({
                                use #root::__private::StructuredArgument as _;
                                #scalars
                                (#expression).__sprintr_argument()
                            })
                        }
                        (Spec::String, false) if field.format == "?" => quote!(
                            #root::__private::BorrowedArgument::Debug(&(#expression))
                        ),
                        (Spec::String, false) => quote!(#root::__private::BorrowedArgument::String(
                            ::core::convert::AsRef::<str>::as_ref(&(#expression))
                        )),
                        (Spec::Hex { .. }, _) => {
                            quote!(#root::__private::BorrowedArgument::Unsigned(
                                #root::__private::unsigned(&(#expression))
                            ))
                        }
                    });
                }
            }
        }
    }
    if let Some(extra) = positional.next() {
        return Err(syn::Error::new_spanned(extra, "unused positional argument"));
    }
    let mut fragments = Vec::new();
    for (index, value) in input.values.iter().enumerate() {
        if index > 0 {
            fragments.push(quote!(b","));
        }
        fragments.push(quote!((#value).__fragment()));
    }
    let values = &input.values;
    let declaration = if nested {
        quote! {
            const __PARTS: &[&[u8]] = &[b"\"", #(#message,)* b"\""];
            static __ENCODED: [u8; #root::__private::encoded_len(__PARTS)] =
                #root::__private::concatenate(__PARTS);
            static __FORMAT: #root::__private::NestedTemplate = #root::__private::NestedTemplate::new(
                &__ENCODED, &[#(#attributes),*], ::core::file!(), ::core::line!(),
            );
        }
    } else {
        quote! {
            const __MODULE: &str = ::core::module_path!();
            const __FILE: &str = ::core::file!();
            const __PARTS: &[&[u8]] = &[
                b"{\"attrs\":{", #(#fragments,)* b"},\"refs\":[", #(#references,)*
                b"],\"msg\":\"", #(#message,)* b"\",\"source\":{\"mod\":",
                &#root::__private::json_string::<{#root::__private::json_string_len(__MODULE)}>(__MODULE),
                b",\"file\":",
                &#root::__private::json_string::<{#root::__private::json_string_len(__FILE)}>(__FILE),
                ::core::concat!(",\"line\":", ::core::line!(), ",\"column\":", ::core::column!(), "}}").as_bytes(),
            ];
            static __ENCODED: [u8; #root::__private::encoded_len(__PARTS)] =
                #root::__private::concatenate(__PARTS);
            static __STATEMENT: #root::__private::Statement = #root::__private::Statement::new(
                &__ENCODED,
                &[#(#attributes),*],
                &[#(&#values),*],
            );
        }
    };
    // Capture each invocation before entering the logger.
    let stack = quote! {
        #root::__private::capture_stack(&mut [0u64; #root::MAX_STACK_DEPTH])
    };
    Ok(Compiled {
        declaration,
        stack,
        arguments,
    })
}

/// Write a statement whose template carries the given static named values.
#[proc_macro]
pub fn record(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as Input);
    let result = compile(&input, false).map(|compiled| {
        let declaration = compiled.declaration;
        let stack = compiled.stack;
        let arguments = compiled.arguments;
        let root = input.root;
        quote! {{ #declaration #root::__private::write_event(&__STATEMENT, #stack, &[#(#arguments),*]) }}
    });
    result.unwrap_or_else(syn::Error::into_compile_error).into()
}

/// A borrowed formatter; invoked only when writing its containing event.
#[proc_macro]
pub fn structured(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as Input);
    let result = compile(&input, true).map(|compiled| {
        let root = &input.root;
        let declaration = compiled.declaration;
        let arguments = compiled.arguments;
        quote! {{
            #declaration
            #root::__private::formatted_value(|__out| {
                __out.__formatted(&__FORMAT, &[#(#arguments),*])
            })
        }}
    });
    result.unwrap_or_else(syn::Error::into_compile_error).into()
}
