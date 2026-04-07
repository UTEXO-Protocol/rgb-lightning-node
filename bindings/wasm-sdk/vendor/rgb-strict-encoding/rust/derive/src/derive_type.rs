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

use amplify_syn::{
    DataInner, DeriveInner, EnumKind, Field, FieldKind, Fields, Items, NamedField, Variant,
};
use proc_macro2::TokenStream as TokenStream2;
use quote::ToTokens;
use syn::Result;

use crate::params::{EnumAttr, FieldAttr, StrictDerive, VariantAttr, VariantTags};

struct DeriveType<'a>(&'a StrictDerive);
struct DeriveProduct;
struct DeriveTuple;
struct DeriveStruct;
struct DeriveSum(EnumAttr);
struct DeriveEnum;
struct DeriveUnion;

impl StrictDerive {
    pub fn derive_type(&self) -> Result<TokenStream2> {
        let trait_crate = &self.conf.strict_crate;
        let type_name = &self.data.name;

        let impl_type = self.data.derive(trait_crate, &ident!(StrictType), &DeriveType(self))?;

        let impl_outer = match &self.data.inner {
            DataInner::Struct(_) => {
                self.data.derive(trait_crate, &ident!(StrictProduct), &DeriveProduct)?
            }
            DataInner::Enum(variants) => {
                let enum_attr = EnumAttr::with(self.data.attr.clone(), variants.enum_kind())?;

                let impl_try_from_u8 = if enum_attr.try_from_u8 {
                    let variant_name = variants.iter().map(|var| &var.name);

                    quote! {
                        #[automatically_derived]
                        impl TryFrom<u8> for #type_name {
                            type Error = #trait_crate::VariantError<u8>;
                            fn try_from(value: u8) -> Result<Self, Self::Error> {
                                match value {
                                    #( x if x == Self::#variant_name as u8 => Ok(Self::#variant_name), )*
                                    wrong => Err(#trait_crate::VariantError::with::<Self>(wrong)),
                                }
                            }
                        }
                    }
                } else {
                    TokenStream2::new()
                };

                let impl_into_u8 = if enum_attr.into_u8 {
                    quote! {
                        #[automatically_derived]
                        impl From<#type_name> for u8 {
                            #[inline]
                            fn from(value: #type_name) -> u8 {
                                value as u8
                            }
                        }
                    }
                } else {
                    TokenStream2::new()
                };

                let impl_struct_enum =
                    self.data.derive(trait_crate, &ident!(StrictSum), &DeriveSum(enum_attr))?;

                quote! {
                    #impl_into_u8
                    #impl_try_from_u8
                    #impl_struct_enum
                }
            }
            _ => TokenStream2::new(),
        };

        let impl_inner = match &self.data.inner {
            DataInner::Struct(Fields::Named(_)) => {
                self.data.derive(trait_crate, &ident!(StrictStruct), &DeriveStruct)?
            }
            DataInner::Struct(Fields::Unnamed(_)) => {
                self.data.derive(trait_crate, &ident!(StrictTuple), &DeriveTuple)?
            }
            DataInner::Enum(variants) if variants.enum_kind() == EnumKind::Primitive => {
                self.data.derive(trait_crate, &ident!(StrictEnum), &DeriveEnum)?
            }
            DataInner::Enum(_) => {
                self.data.derive(trait_crate, &ident!(StrictUnion), &DeriveUnion)?
            }
            _ => TokenStream2::new(),
        };

        Ok(quote! {
            #impl_type
            #impl_outer
            #impl_inner
        })
    }
}

impl DeriveType<'_> {
    pub fn derive_type(&self) -> Result<TokenStream2> {
        let crate_name = &self.0.conf.strict_crate;
        let lib_name = &self.0.conf.lib;

        let strict_name = match self.0.conf.rename {
            Some(ref rename) => quote! {
                fn strict_name() -> Option<#crate_name::TypeName> {
                    Some(tn!(#rename))
                }
            },
            None => TokenStream2::new(),
        };

        Ok(quote! {
            const STRICT_LIB_NAME: &'static str = #lib_name;

            #strict_name
        })
    }
}

impl DeriveInner for DeriveType<'_> {
    fn derive_unit_inner(&self) -> Result<TokenStream2> { self.derive_type() }
    fn derive_struct_inner(&self, _fields: &Items<NamedField>) -> Result<TokenStream2> {
        self.derive_type()
    }
    fn derive_tuple_inner(&self, _fields: &Items<Field>) -> Result<TokenStream2> {
        self.derive_type()
    }
    fn derive_enum_inner(&self, _variants: &Items<Variant>) -> Result<TokenStream2> {
        self.derive_type()
    }
}

impl DeriveInner for DeriveProduct {
    fn derive_unit_inner(&self) -> Result<TokenStream2> { unreachable!() }
    fn derive_enum_inner(&self, _variants: &Items<Variant>) -> Result<TokenStream2> {
        unreachable!()
    }

    fn derive_struct_inner(&self, _fields: &Items<NamedField>) -> Result<TokenStream2> {
        Ok(TokenStream2::new())
    }
    fn derive_tuple_inner(&self, _fields: &Items<Field>) -> Result<TokenStream2> {
        Ok(TokenStream2::new())
    }
}

impl DeriveInner for DeriveSum {
    fn derive_unit_inner(&self) -> Result<TokenStream2> { unreachable!() }
    fn derive_struct_inner(&self, _fields: &Items<NamedField>) -> Result<TokenStream2> {
        unreachable!()
    }
    fn derive_tuple_inner(&self, _fields: &Items<Field>) -> Result<TokenStream2> { unreachable!() }

    fn derive_enum_inner(&self, variants: &Items<Variant>) -> Result<TokenStream2> {
        let mut orders = Vec::with_capacity(variants.len());
        let mut idents = Vec::with_capacity(variants.len());
        let mut renames = Vec::with_capacity(variants.len());

        for (index, variant) in variants.iter().enumerate() {
            let attr = VariantAttr::try_from(variant.attr.clone())?;
            let name = &variant.name;
            let rename = attr.variant_name(name);
            let tag = match (&self.0.tags, &attr.tag) {
                (_, Some(tag)) => tag.to_token_stream(),
                (VariantTags::Repr, None) => quote! { Self::#name },
                (VariantTags::Order, None) => quote! { #index },
                (VariantTags::Custom, None) => {
                    panic!("tag is required for variant `{}`", variant.name)
                }
            };
            orders.push(quote!(#tag));
            renames.push(quote!(#rename));
            idents.push(match variant.fields {
                Fields::Unit => quote!(#name),
                Fields::Named(_) => quote!(#name { .. }),
                Fields::Unnamed(_) => quote!(#name(..)),
            });
        }

        Ok(quote! {
            const ALL_VARIANTS: &'static [(u8, &'static str)] = &[
                #( (#orders as u8, #renames) ),*
            ];

            fn variant_name(&self) -> &'static str {
                match self {
                    #( Self::#idents => #renames, )*
                }
            }
        })
    }
}

impl DeriveInner for DeriveTuple {
    fn derive_unit_inner(&self) -> Result<TokenStream2> { unreachable!() }
    fn derive_struct_inner(&self, _fields: &Items<NamedField>) -> Result<TokenStream2> {
        unreachable!()
    }
    fn derive_enum_inner(&self, _variants: &Items<Variant>) -> Result<TokenStream2> {
        unreachable!()
    }

    fn derive_tuple_inner(&self, fields: &Items<Field>) -> Result<TokenStream2> {
        let field_count = fields.len();
        Ok(quote! {
            const FIELD_COUNT: u8 = #field_count as u8;
        })
    }
}

impl DeriveInner for DeriveStruct {
    fn derive_unit_inner(&self) -> Result<TokenStream2> { unreachable!() }
    fn derive_enum_inner(&self, _variants: &Items<Variant>) -> Result<TokenStream2> {
        unreachable!()
    }
    fn derive_tuple_inner(&self, _fields: &Items<Field>) -> Result<TokenStream2> { unreachable!() }

    fn derive_struct_inner(&self, fields: &Items<NamedField>) -> Result<TokenStream2> {
        let mut name = Vec::with_capacity(fields.len());
        for named_field in fields {
            let attr = FieldAttr::with(named_field.field.attr.clone(), FieldKind::Named)?;
            if !attr.skip {
                name.push(attr.field_name(&named_field.name));
            }
        }

        Ok(quote! {
            const ALL_FIELDS: &'static [&'static str] = &[
                #( #name ),*
            ];
        })
    }
}

impl DeriveInner for DeriveEnum {
    fn derive_unit_inner(&self) -> Result<TokenStream2> { unreachable!() }
    fn derive_struct_inner(&self, _fields: &Items<NamedField>) -> Result<TokenStream2> {
        unreachable!()
    }
    fn derive_tuple_inner(&self, _fields: &Items<Field>) -> Result<TokenStream2> { unreachable!() }

    fn derive_enum_inner(&self, _variants: &Items<Variant>) -> Result<TokenStream2> {
        Ok(TokenStream2::new())
    }
}

impl DeriveInner for DeriveUnion {
    fn derive_unit_inner(&self) -> Result<TokenStream2> { unreachable!() }
    fn derive_struct_inner(&self, _fields: &Items<NamedField>) -> Result<TokenStream2> {
        unreachable!()
    }
    fn derive_tuple_inner(&self, _fields: &Items<Field>) -> Result<TokenStream2> { unreachable!() }

    fn derive_enum_inner(&self, _variants: &Items<Variant>) -> Result<TokenStream2> {
        Ok(TokenStream2::new())
    }
}
