//! Generate the C declaration subset directly from ffi.rs; unsupported types fail the build.

use std::{env, fmt::Write as _, fs, path::PathBuf};
use syn::{Fields, Item, ReturnType, Type};

fn c_type(ty: &Type) -> String {
    match ty {
        Type::Path(path) => match path
            .path
            .get_ident()
            .expect("simple C ABI type")
            .to_string()
            .as_str()
        {
            "u8" => "uint8_t".into(),
            "u32" => "uint32_t".into(),
            "usize" => "size_t".into(),
            name if name.starts_with("Wxsl") => name.into(),
            other => panic!("unsupported C ABI type {other}"),
        },
        Type::Ptr(ptr) => format!(
            "{}{} *",
            if ptr.const_token.is_some() {
                "const "
            } else {
                ""
            },
            c_type(&ptr.elem)
        ),
        _ => panic!("unsupported C ABI type"),
    }
}

fn docs(attributes: &[syn::Attribute], out: &mut String) {
    for attr in attributes {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let syn::Meta::NameValue(value) = &attr.meta {
            if let syn::Expr::Lit(lit) = &value.value {
                if let syn::Lit::Str(line) = &lit.lit {
                    writeln!(out, "/* {} */", line.value().trim().replace("*/", "* /")).unwrap();
                }
            }
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=src/lib.rs");
    let source = fs::read_to_string("src/ffi.rs").unwrap();
    let syntax = syn::parse_file(&source).unwrap();
    let lib = syn::parse_file(&fs::read_to_string("src/lib.rs").unwrap()).unwrap();
    let abi = lib
        .items
        .iter()
        .find_map(|item| match item {
            Item::Const(item) if item.ident == "ABI_VERSION" => Some(&item.expr),
            _ => None,
        })
        .expect("ABI_VERSION constant");
    let syn::Expr::Lit(lit) = &**abi else {
        panic!("literal ABI version")
    };
    let syn::Lit::Int(abi) = &lit.lit else {
        panic!("integer ABI version")
    };
    let mut header = format!("/* Generated from wxsl-ffi/src/ffi.rs. Do not edit. */\n#ifndef WXSL_H\n#define WXSL_H\n#include <stddef.h>\n#include <stdint.h>\n#define WXSL_ABI_VERSION {}u\n#ifdef __cplusplus\nextern \"C\" {{\n#endif\n\ntypedef struct WxslResult WxslResult;\n", abi.base10_digits());
    for item in &syntax.items {
        match item {
            Item::Enum(item) if item.ident == "WxslStatus" => {
                docs(&item.attrs, &mut header);
                header.push_str("typedef uint32_t WxslStatus;\nenum {\n");
                for variant in &item.variants {
                    let (_, syn::Expr::Lit(lit)) =
                        variant.discriminant.as_ref().expect("explicit status code")
                    else {
                        panic!("literal status")
                    };
                    let syn::Lit::Int(value) = &lit.lit else {
                        panic!("integer status")
                    };
                    writeln!(
                        header,
                        "    WXSL_STATUS_{} = {},",
                        variant.ident.to_string().chars().enumerate().fold(
                            String::new(),
                            |mut out, (i, ch)| {
                                if i > 0 && ch.is_uppercase() {
                                    out.push('_');
                                }
                                out.push(ch.to_ascii_uppercase());
                                out
                            }
                        ),
                        value.base10_digits()
                    )
                    .unwrap();
                }
                header.push_str("};\n\n");
            }
            Item::Struct(item) if item.attrs.iter().any(|attr| attr.path().is_ident("repr")) => {
                docs(&item.attrs, &mut header);
                writeln!(header, "typedef struct {} {{", item.ident).unwrap();
                let Fields::Named(fields) = &item.fields else {
                    panic!("named ABI fields")
                };
                for field in &fields.named {
                    docs(&field.attrs, &mut header);
                    writeln!(
                        header,
                        "    {} {};",
                        c_type(&field.ty),
                        field.ident.as_ref().unwrap()
                    )
                    .unwrap();
                }
                writeln!(header, "}} {};\n", item.ident).unwrap();
            }
            Item::Fn(item)
                if item
                    .attrs
                    .iter()
                    .any(|attr| attr.path().is_ident("no_mangle")) =>
            {
                assert_eq!(
                    item.sig
                        .abi
                        .as_ref()
                        .and_then(|abi| abi.name.as_ref())
                        .map(syn::LitStr::value)
                        .as_deref(),
                    Some("C")
                );
                docs(&item.attrs, &mut header);
                let ret = match &item.sig.output {
                    ReturnType::Default => "void".into(),
                    ReturnType::Type(_, ty) => c_type(ty),
                };
                let args = item
                    .sig
                    .inputs
                    .iter()
                    .map(|arg| {
                        let syn::FnArg::Typed(arg) = arg else {
                            panic!("no receiver")
                        };
                        let syn::Pat::Ident(name) = &*arg.pat else {
                            panic!("named ABI argument")
                        };
                        format!("{} {}", c_type(&arg.ty), name.ident)
                    })
                    .collect::<Vec<_>>();
                writeln!(
                    header,
                    "{ret} {}({});\n",
                    item.sig.ident,
                    if args.is_empty() {
                        "void".into()
                    } else {
                        args.join(", ")
                    }
                )
                .unwrap();
                writeln!(
                    header,
                    "typedef {ret} (*wxsl_fn_{})({});\n",
                    item.sig.ident,
                    if args.is_empty() {
                        "void".into()
                    } else {
                        args.join(", ")
                    }
                )
                .unwrap();
            }
            _ => {}
        }
    }
    header.push_str("#ifdef __cplusplus\n}\n#endif\n#endif\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("wxsl.h"),
        header,
    )
    .unwrap();
}
