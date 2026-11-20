// SPDX-License-Identifier: Apache-2.0

use proc_macro::TokenStream;
use quote::{ToTokens, quote};
use slogr_message::format::{Part, Spec, parse};
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

struct Compiled {
    format: LitStr,
    prefix: LitByteStr,
    owned: Vec<proc_macro2::TokenStream>,
    borrowed: Vec<proc_macro2::TokenStream>,
    bindings: Vec<proc_macro2::TokenStream>,
}

fn compile(input: &Input) -> syn::Result<Compiled> {
    let root = &input.root;
    let error = |message| syn::Error::new(input.format.span(), message);
    let parts = parse(&input.format.value()).map_err(|err| error(err.to_string()))?;
    let mut arguments = input.arguments.iter();
    let mut owned = Vec::new();
    let mut borrowed = Vec::new();
    let mut bindings = Vec::new();
    let mut stored = String::new();
    for part in parts {
        match part {
            Part::Literal(text) => {
                stored.push_str(&text.replace('{', "{{").replace('}', "}}"));
            }
            Part::Field(field) => {
                let (label, expression, attribute) = if field.name.is_empty() {
                    let expression = arguments
                        .next()
                        .ok_or_else(|| error("missing positional argument".into()))?;
                    if let Expr::Assign(assignment) = expression {
                        let Expr::Path(attribute) = assignment.left.as_ref() else {
                            return Err(syn::Error::new_spanned(
                                &assignment.left,
                                "expected a registered attribute handle",
                            ));
                        };
                        let label = attribute
                            .path
                            .segments
                            .last()
                            .expect("attribute path")
                            .ident
                            .to_string();
                        let value = &assignment.right;
                        (label, quote!(#value), Some(quote!(#attribute)))
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
                    let mut ident = syn::parse_str::<Ident>(&field.name).map_err(|_| {
                        error("captured placeholders must name a local variable".into())
                    })?;
                    ident.set_span(input.format.span());
                    (field.name, quote!(#ident), None)
                };
                if field.format.is_empty() && attribute.is_none() {
                    return Err(error(
                        "use :s for strings or :x for unsigned integers".into(),
                    ));
                }
                if attribute.is_some() && field.spec != Spec::String {
                    return Err(error(
                        "registered string attributes require {} or {:s}".into(),
                    ));
                }
                let string_value = if let Some(attribute) = &attribute {
                    let argument = owned.len();
                    bindings.push(quote! {
                        #root::__private::AttributeBinding {
                            attribute: &(#attribute),
                            argument: #argument,
                        }
                    });
                    quote! { (#attribute).__value(&(#expression)) }
                } else {
                    quote! { ::core::convert::AsRef::<str>::as_ref(&(#expression)) }
                };
                stored.push('{');
                stored.push_str(&label);
                if field.spec != Spec::String {
                    stored.push(':');
                    stored.push_str(&field.format);
                }
                stored.push('}');
                owned.push(match field.spec {
                    Spec::String => quote! {
                        #root::Argument::String(
                            ::std::string::String::from(
                                #string_value
                            )
                        )
                    },
                    Spec::Hex { .. } => quote! {
                        #root::Argument::Unsigned(#root::__private::unsigned(&(#expression)))
                    },
                });
                borrowed.push(match field.spec {
                    Spec::String => quote! {
                        #root::__private::BorrowedArgument::String(
                            #string_value
                        )
                    },
                    Spec::Hex { .. } => quote! {
                        #root::__private::BorrowedArgument::Unsigned(
                            #root::__private::unsigned(&(#expression))
                        )
                    },
                });
            }
        }
    }
    if let Some(extra) = arguments.next() {
        return Err(syn::Error::new_spanned(extra, "unused positional argument"));
    }
    let json = serde_json::to_string(&stored).map_err(|err| error(err.to_string()))?;
    let prefix = format!("{{\"msg\":{json},\"args\":");
    Ok(Compiled {
        format: LitStr::new(&stored, input.format.span()),
        prefix: LitByteStr::new(prefix.as_bytes(), input.format.span()),
        owned,
        borrowed,
        bindings,
    })
}

/// Internal entry point; use the public wrapper in `slogr-prototype`.
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
            let format = compiled.format;
            let arguments = compiled.owned;
            let bindings = compiled.bindings;
            quote! {
                #root::__private::owned_message(
                    #format, ::std::vec![#(#arguments),*], &[#(#bindings),*],
                )
            }
        })
    };
    result.unwrap_or_else(syn::Error::into_compile_error).into()
}

/// Internal entry point; use the public wrapper in `slogr-prototype`.
#[proc_macro]
pub fn info(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as Input);
    let result = compile(&input).map(|compiled| {
        let prefix = compiled.prefix;
        let arguments = compiled.borrowed;
        let bindings = compiled.bindings;
        if let Some(logger) = input.logger {
            quote! { (#logger).__write_compiled(#prefix, &[#(#arguments),*], &[#(#bindings),*]) }
        } else {
            let root = input.root;
            quote! { #root::__private::write_stdout(#prefix, &[#(#arguments),*], &[#(#bindings),*]) }
        }
    });
    result.unwrap_or_else(syn::Error::into_compile_error).into()
}
