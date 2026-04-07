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

#[macro_use]
extern crate amplify;
#[macro_use]
extern crate strict_encoding_derive;

mod common;

use std::convert::Infallible;

use strict_encoding::{
    StrictDecode, StrictDumb, StrictEncode, StrictSerialize, StrictSum, VariantError,
};

const TEST_LIB: &str = "TestLib";

#[test]
fn wrapper_base() -> common::Result {
    #[derive(Clone, PartialEq, Eq, Debug)]
    #[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
    #[strict_type(lib = TEST_LIB)]
    struct ShortLen(u16);

    impl StrictSerialize for ShortLen {}

    assert_eq!(ShortLen(7).to_strict_serialized::<2>().unwrap().as_slice(), &[7, 0]);

    Ok(())
}

#[test]
fn tuple_base() -> common::Result {
    #[derive(Clone, PartialEq, Eq, Debug)]
    #[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
    #[strict_type(lib = TEST_LIB)]
    struct TaggedInfo(u16, u64);

    impl StrictSerialize for TaggedInfo {}

    assert_eq!(TaggedInfo(7, 9).to_strict_serialized::<10>().unwrap().as_slice(), &[
        7, 0, 9, 0, 0, 0, 0, 0, 0, 0
    ]);

    Ok(())
}

#[test]
fn tuple_generics() -> common::Result {
    #[derive(Clone, PartialEq, Eq, Debug)]
    #[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
    #[strict_type(lib = TEST_LIB)]
    struct Pair<
        A: StrictDumb + StrictEncode + StrictDecode,
        B: StrictDumb + StrictEncode + StrictDecode,
    >(A, B);

    impl<
            A: StrictDumb + StrictEncode + StrictDecode,
            B: StrictDumb + StrictEncode + StrictDecode,
        > StrictSerialize for Pair<A, B>
    {
    }

    assert_eq!(Pair(7, 9).to_strict_serialized::<8>().unwrap().as_slice(), &[
        7, 0, 0, 0, 9, 0, 0, 0
    ]);

    #[derive(Clone, PartialEq, Eq, Debug)]
    #[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
    #[strict_type(lib = TEST_LIB)]
    struct WhereConstraint<A: TryInto<u8>, B: From<i32>>(A, B)
    where
        A: StrictDumb + StrictEncode + StrictDecode + From<u8>,
        <A as TryFrom<u8>>::Error: From<Infallible>,
        B: StrictDumb + StrictEncode + StrictDecode;

    impl<A: TryInto<u8>, B: From<i32>> StrictSerialize for WhereConstraint<A, B>
    where
        A: StrictDumb + StrictEncode + StrictDecode + From<u8>,
        <A as TryFrom<u8>>::Error: From<Infallible>,
        B: StrictDumb + StrictEncode + StrictDecode,
    {
    }

    assert_eq!(WhereConstraint(7, 9).to_strict_serialized::<8>().unwrap().as_slice(), &[
        7, 0, 0, 0, 9, 0, 0, 0
    ]);

    Ok(())
}

#[test]
fn struct_generics() -> common::Result {
    #[derive(Clone, PartialEq, Eq, Debug)]
    #[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
    #[strict_type(lib = TEST_LIB)]
    struct Field<V: StrictEncode + StrictDecode + StrictDumb> {
        tag: u8,
        value: V,
    }

    impl<V: StrictEncode + StrictDecode + StrictDumb> StrictSerialize for Field<V> {}

    assert_eq!(Field { tag: 7, value: 9 }.to_strict_serialized::<5>().unwrap().as_slice(), &[
        7, 9, 0, 0, 0
    ]);

    Ok(())
}

#[test]
fn enum_ord() -> common::Result {
    #[derive(Copy, Clone, PartialEq, Eq, Debug)]
    #[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
    #[strict_type(lib = TEST_LIB, tags = repr, into_u8, try_from_u8)]
    #[repr(u8)]
    enum Variants {
        #[strict_type(dumb)]
        One = 5,
        Two = 6,
        Three = 7,
    }

    assert_eq!(Variants::Three as u8, 7);
    assert_eq!(u8::from(Variants::Three), 7);
    assert_eq!(Variants::try_from(6), Ok(Variants::Two));
    assert_eq!(Variants::try_from(3), Err(VariantError(Some(s!("Variants")), 3)));

    Ok(())
}

#[test]
fn enum_repr() -> common::Result {
    #[derive(Copy, Clone, PartialEq, Eq, Debug)]
    #[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
    #[strict_type(lib = TEST_LIB, tags = repr, into_u8, try_from_u8)]
    #[repr(u16)]
    enum Cls {
        One = 1,
        #[strict_type(dumb)]
        Two,
        Three,
    }

    assert_eq!(u8::from(Cls::Three), 3);
    assert_eq!(Cls::try_from(2), Ok(Cls::Two));
    assert_eq!(Cls::try_from(4), Err(VariantError(Some(s!("Cls")), 4)));

    Ok(())
}

#[test]
fn enum_associated() -> common::Result {
    #[allow(dead_code)]
    #[derive(Copy, Clone, PartialEq, Eq, Debug)]
    #[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
    #[strict_type(lib = TEST_LIB, tags = order)]
    enum Assoc {
        One {
            hash: [u8; 32],
            ord: u8,
        },
        Two(u8, u16, u32),
        #[strict_type(dumb)]
        Three,
        Four(),
        Five {},
    }

    assert_eq!(Assoc::ALL_VARIANTS, &[
        (0, "one"),
        (1, "two"),
        (2, "three"),
        (3, "four"),
        (4, "five")
    ]);

    Ok(())
}

#[test]
fn enum_custom_tags() -> common::Result {
    #[allow(dead_code)]
    #[derive(Copy, Clone, PartialEq, Eq, Debug)]
    #[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
    #[strict_type(lib = TEST_LIB, tags = order)]
    enum Assoc {
        One {
            hash: [u8; 32],
            ord: u8,
        },
        #[strict_type(tag = 2)]
        Two(u8, u16, u32),
        #[strict_type(dumb, tag = 3)]
        Three,
        #[strict_type(tag = 4)]
        Four(),
        #[strict_type(tag = 5)]
        Five {},
    }

    impl StrictSerialize for Assoc {}

    assert_eq!(Assoc::ALL_VARIANTS, &[
        (0, "one"),
        (2, "two"),
        (3, "three"),
        (4, "four"),
        (5, "five")
    ]);

    let assoc = Assoc::Two(0, 1, 2);
    assert_eq!(assoc.to_strict_serialized::<256>().unwrap().as_slice(), &[2, 0, 1, 0, 2, 0, 0, 0]);

    let assoc = Assoc::One {
        hash: [0u8; 32],
        ord: 0,
    };
    assert_eq!(assoc.to_strict_serialized::<256>().unwrap().as_slice(), &[0u8; 34]);

    Ok(())
}
