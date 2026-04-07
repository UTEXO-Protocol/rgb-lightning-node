// Derivation macro library for strict encoding.
//
// SPDX-License-Identifier: Apache-2.0
//
// Designed in 2019-2025 by Dr Maxim Orlovsky <orlovsky@ubideco.org>
// Written in 2024-2025 by Dr Maxim Orlovsky <orlovsky@ubideco.org>
//
// Copyright (C) 2022-2025 Laboratories for Ubiquitous Deterministic Computing (UBIDECO),
//                         Institute for Distributed and Cognitive Systems (InDCS), Switzerland.
// Copyright (C) 2022-2025 Dr Maxim Orlovsky.
// All rights under the above copyrights are reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use this file except
// in compliance with the License. You may obtain a copy of the License at
//
//        http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software distributed under the License
// is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express
// or implied. See the License for the specific language governing permissions and limitations under
// the License.

use amplify_syn::{DeriveInner, EnumKind, Field, FieldKind, Fields, Items, NamedField, Variant};
use proc_macro2::{Ident, Span, TokenStream as TokenStream2};
use syn::{Error, Result};

use crate::params::{FieldAttr, StrictDerive, VariantAttr};

struct DeriveDecode<'a>(&'a StrictDerive);

impl StrictDerive {
    pub fn derive_decode(&self) -> Result<TokenStream2> {
        let res = self.data.derive(
            &self.conf.strict_crate,
            &ident!(StrictDecode),
            &DeriveDecode(self),
        )?;
        Ok(res)
    }
}

fn derive_struct_fields(
    fields: &Items<NamedField>,
    self_name: TokenStream2,
) -> Result<TokenStream2> {
    let mut skipped = Vec::new();
    let mut field_name = Vec::with_capacity(fields.len());
    let mut field_rename = Vec::with_capacity(fields.len());
    for named_field in fields {
        let attr = FieldAttr::with(named_field.field.attr.clone(), FieldKind::Named)?;

        let name = &named_field.name;
        let rename = attr.field_name(name);

        if attr.skip {
            skipped.push(quote! { #name })
        } else {
            field_name.push(quote! { #name });
            field_rename.push(quote! { #rename });
        }
    }
    Ok(quote! {
        #( let #field_name = r.read_field(fname!(#field_rename))?; )*
        Ok(#self_name {
            #(#field_name,)*
            #(#skipped: Default::default()),*
        })
    })
}

fn derive_tuple_fields(fields: &Items<Field>, self_name: TokenStream2) -> Result<TokenStream2> {
    let mut field_idx = Vec::with_capacity(fields.len());
    let mut field_vars = Vec::with_capacity(fields.len());
    for (index, field) in fields.iter().enumerate() {
        let attr = FieldAttr::with(field.attr.clone(), FieldKind::Unnamed)?;
        if attr.skip {
            field_vars.push(quote! { Default::default() });
        } else {
            let index = Ident::new(&format!("_{index}"), Span::call_site());
            field_idx.push(quote! { #index });
            field_vars.push(quote! { #index });
        }
    }
    Ok(quote! {
        #( let #field_idx = r.read_field()?; )*
        Ok(#self_name( #( #field_vars ),* ))
    })
}

impl DeriveInner for DeriveDecode<'_> {
    fn derive_unit_inner(&self) -> Result<TokenStream2> {
        Err(Error::new(
            Span::call_site(),
            "StrictDecode must not be derived on a unit types. Use just a unit type instead when \
             encoding parent structure.",
        ))
    }

    fn derive_struct_inner(&self, fields: &Items<NamedField>) -> Result<TokenStream2> {
        let crate_name = &self.0.conf.strict_crate;
        let inner = derive_struct_fields(fields, quote! { Self })?;
        Ok(quote! {
            fn strict_decode(reader: &mut impl #crate_name::TypedRead) -> Result<Self, #crate_name::DecodeError> {
                use #crate_name::{TypedRead, ReadStruct, fname};
                reader.read_struct(|r| {
                    #inner
                })
            }
        })
    }

    fn derive_tuple_inner(&self, fields: &Items<Field>) -> Result<TokenStream2> {
        let crate_name = &self.0.conf.strict_crate;
        let inner = derive_tuple_fields(fields, quote! { Self })?;
        Ok(quote! {
            fn strict_decode(reader: &mut impl #crate_name::TypedRead) -> Result<Self, #crate_name::DecodeError> {
                use #crate_name::{TypedRead, ReadTuple};
                reader.read_tuple(|r| {
                    #inner
                })
            }
        })
    }

    fn derive_enum_inner(&self, variants: &Items<Variant>) -> Result<TokenStream2> {
        let crate_name = &self.0.conf.strict_crate;

        let inner = if variants.enum_kind() == EnumKind::Primitive {
            quote! {
                reader.read_enum()
            }
        } else {
            let mut read_variants = Vec::with_capacity(variants.len());
            for var in variants {
                let attr = VariantAttr::try_from(var.attr.clone())?;
                let var_name = &var.name;
                let name = attr.variant_name(var_name);
                match &var.fields {
                    Fields::Unit => {
                        read_variants.push(quote! {
                            #name => Ok(Self::#var_name),
                        });
                    }
                    Fields::Unnamed(fields) if fields.is_empty() => {
                        read_variants.push(quote! {
                            #name => Ok(Self::#var_name()),
                        });
                    }
                    Fields::Named(fields) if fields.is_empty() => {
                        read_variants.push(quote! {
                            #name => Ok(Self::#var_name {}),
                        });
                    }
                    Fields::Unnamed(fields) => {
                        let inner = derive_tuple_fields(fields, quote! { Self::#var_name })?;
                        read_variants.push(quote! {
                            #name => r.read_tuple(|r| {
                                #inner
                            }),
                        });
                    }
                    Fields::Named(fields) => {
                        let inner = derive_struct_fields(fields, quote! { Self::#var_name })?;
                        read_variants.push(quote! {
                            #name => r.read_struct(|r| {
                                #inner
                            }),
                        });
                    }
                }
            }

            quote! {
                #[allow(unused_imports)]
                use #crate_name::{ReadUnion, ReadTuple, ReadStruct, fname};
                reader.read_union(|field_name, r| {
                    match field_name.as_str() {
                        #( #read_variants )*
                        _ => unreachable!(),
                    }
                })
            }
        };

        Ok(quote! {
            fn strict_decode(reader: &mut impl #crate_name::TypedRead) -> Result<Self, #crate_name::DecodeError> {
                use #crate_name::TypedRead;
                #inner
            }
        })
    }
}
