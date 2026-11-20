// SPDX-License-Identifier: Apache-2.0

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as Tokens};
use quote::{ToTokens, quote};
use sprintr_message::format::{Part, Spec, parse};
use sprintr_message::{RelationDefinition, RelationKind, RelationType, validate_attribute_name};
use syn::parse::{Parse, ParseStream};
use syn::{Expr, Ident, LitByteStr, LitStr, Path, Token, parse_macro_input};

struct Input {
    root: Path,
    format: LitStr,
    logger: Option<Expr>,
    arguments: Vec<Expr>,
}

impl Parse for Input {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let root = input.parse()?;
        input.parse::<Token![;]>()?;
        let format = input.parse()?;
        let mut logger = None;
        let mut arguments = Vec::new();
        while !input.is_empty() {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let fork = input.fork();
            if fork.parse::<Ident>().is_ok_and(|ident| ident == "logger")
                && fork.peek(Token![:])
                && !fork.peek(Token![::])
            {
                if logger.is_some() || !arguments.is_empty() {
                    return Err(
                        input.error("logger: must appear once, immediately after the format")
                    );
                }
                input.parse::<Ident>()?;
                input.parse::<Token![:]>()?;
                logger = Some(input.parse()?);
            } else {
                arguments.push(input.parse()?);
            }
        }
        Ok(Self {
            root,
            format,
            logger,
            arguments,
        })
    }
}

struct AttributeInput {
    root: Path,
    name: LitStr,
    description: LitStr,
}

impl Parse for AttributeInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let root = input.parse()?;
        input.parse::<Token![;]>()?;
        let name = input.parse()?;
        input.parse::<Token![,]>()?;
        let description = input.parse()?;
        if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
        }
        Ok(Self {
            root,
            name,
            description,
        })
    }
}

fn bytes(text: &str, span: Span) -> LitByteStr {
    LitByteStr::new(text.as_bytes(), span)
}

fn json_content(text: &str, span: Span) -> LitByteStr {
    let encoded = serde_json::to_string(text).expect("string JSON encoding");
    bytes(&encoded[1..encoded.len() - 1], span)
}

/// Pre-encode an immutable, statically declared string attribute.
#[proc_macro]
pub fn attribute(input: TokenStream) -> TokenStream {
    let AttributeInput {
        root,
        name,
        description,
    } = parse_macro_input!(input as AttributeInput);
    if let Err(error) = validate_attribute_name(&name.value()) {
        return syn::Error::new(name.span(), error)
            .into_compile_error()
            .into();
    }
    let key = serde_json::to_string(&format!("log.relation:{}", name.value()))
        .expect("string JSON encoding");
    let definition = RelationDefinition {
        kind: RelationKind::Attribute,
        value_type: RelationType::String,
        desc: description.value(),
        anonymous: false,
    };
    let definition = bytes(
        &format!(
            "{key}:{}",
            serde_json::to_string(&definition).expect("relation JSON encoding")
        ),
        name.span(),
    );
    let label = json_content(&format!("@{}", name.value()), name.span());
    let reference = bytes(
        &format!(
            "{{{}:{{\"$arg\":",
            serde_json::to_string(&name.value()).expect("string JSON encoding")
        ),
        name.span(),
    );
    quote! { #root::Attribute::__new(#name, #description, #definition, #label, #reference) }.into()
}

struct Compiled {
    declaration: Tokens,
    owned: Vec<Tokens>,
    borrowed: Vec<Tokens>,
}

fn compile(input: &Input) -> syn::Result<Compiled> {
    let root = &input.root;
    let span = input.format.span();
    let error = |message: &str| syn::Error::new(span, message);
    let parts = parse(&input.format.value()).map_err(|err| error(&err.to_string()))?;
    let mut arguments = input.arguments.iter();
    let mut owned = Vec::new();
    let mut borrowed = Vec::new();
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
                    let expression = arguments
                        .next()
                        .ok_or_else(|| error("missing positional argument"))?;
                    if let Expr::Assign(assignment) = expression {
                        let Expr::Path(attribute) = assignment.left.as_ref() else {
                            return Err(syn::Error::new_spanned(
                                &assignment.left,
                                "expected a static attribute path",
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
                if field.format.is_empty() && attribute.is_none() {
                    return Err(error("use :s for strings or :x for unsigned integers"));
                }
                if attribute.is_some() && field.spec != Spec::String {
                    return Err(error("static string attributes require {} or {:s}"));
                }
                let string_value = if let Some(attribute) = &attribute {
                    if !attributes.is_empty() {
                        references.push(quote!(b","));
                    }
                    let index = bytes(&format!("{}}}}}", owned.len()), span);
                    references.push(quote!((#attribute).__reference_prefix()));
                    references.push(quote!(#index));
                    attributes.push(quote!(&(#attribute)));
                    message.push(quote!(b"{"));
                    message.push(quote!((#attribute).__format_name()));
                    message.push(quote!(b"}"));
                    quote!((#attribute).__value(&(#expression)))
                } else {
                    let suffix = if field.spec == Spec::String {
                        String::new()
                    } else {
                        format!(":{}", field.format)
                    };
                    let label = json_content(&format!("{{{label}{suffix}}}"), span);
                    message.push(quote!(#label));
                    quote!(::core::convert::AsRef::<str>::as_ref(&(#expression)))
                };
                owned.push(match field.spec {
                    Spec::String => quote!(#root::Argument::String(::std::string::String::from(#string_value))),
                    Spec::Hex { .. } => quote!(#root::Argument::Unsigned(#root::__private::unsigned(&(#expression)))),
                });
                borrowed.push(match field.spec {
                    Spec::String => quote!(#root::__private::BorrowedArgument::String(#string_value)),
                    Spec::Hex { .. } => quote!(#root::__private::BorrowedArgument::Unsigned(#root::__private::unsigned(&(#expression)))),
                });
            }
        }
    }
    if let Some(extra) = arguments.next() {
        return Err(syn::Error::new_spanned(extra, "unused positional argument"));
    }
    let declaration = quote! {
        const __PARTS: &[&[u8]] = &[
            b"{\"attrs\":{},\"refs\":[", #(#references,)* b"],\"msg\":\"", #(#message,)* b"\"}",
        ];
        static __ENCODED: [u8; #root::__private::encoded_len(__PARTS)] =
            #root::__private::concatenate(__PARTS);
        static __STATEMENT: #root::__private::Statement =
            #root::__private::Statement::new(&__ENCODED, &[#(#attributes),*]);
    };
    Ok(Compiled {
        declaration,
        owned,
        borrowed,
    })
}

#[proc_macro]
pub fn message(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as Input);
    let result = if let Some(logger) = &input.logger {
        Err(syn::Error::new_spanned(
            logger,
            "message! constructs a record; use info! for logger output",
        ))
    } else {
        compile(&input).map(|compiled| {
            let root = &input.root;
            let declaration = compiled.declaration;
            let arguments = compiled.owned;
            quote! {{
                #declaration
                #root::__private::owned_message(&__STATEMENT, ::std::vec![#(#arguments),*])
            }}
        })
    };
    result.unwrap_or_else(syn::Error::into_compile_error).into()
}

#[proc_macro]
pub fn info(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as Input);
    let result = compile(&input).map(|compiled| {
        let declaration = compiled.declaration;
        let arguments = compiled.borrowed;
        let call = if let Some(logger) = input.logger {
            quote!((#logger).__write_compiled(&__STATEMENT, &[#(#arguments),*]))
        } else {
            let root = input.root;
            quote!(#root::__private::write_stdout(&__STATEMENT, &[#(#arguments),*]))
        };
        quote! {{ #declaration #call }}
    });
    result.unwrap_or_else(syn::Error::into_compile_error).into()
}
