// Strict encoding library for deterministic binary serialization.
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

use std::cmp::Ordering;
use std::fmt::{self, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::io;

use crate::{ReadStruct, VariantName, WriteStruct, STRICT_TYPES_LIB};

// TODO: Control that min > max!
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(crate = "serde_crate"))]
pub struct Sizing {
    pub min: u64,
    pub max: u64,
}
impl_strict_struct!(Sizing, STRICT_TYPES_LIB; min, max);

impl Sizing {
    // TODO: Remove (in strict-types use Array with size 1 for char types instead).
    pub const ONE: Sizing = Sizing { min: 1, max: 1 };

    pub const U8: Sizing = Sizing {
        min: 0,
        max: u8::MAX as u64,
    };

    pub const U16: Sizing = Sizing {
        min: 0,
        max: u16::MAX as u64,
    };

    pub const U8_NONEMPTY: Sizing = Sizing {
        min: 1,
        max: u8::MAX as u64,
    };

    pub const U16_NONEMPTY: Sizing = Sizing {
        min: 1,
        max: u16::MAX as u64,
    };

    pub const fn new(min: u64, max: u64) -> Self { Sizing { min, max } }

    pub const fn fixed(len: u64) -> Self { Sizing { min: len, max: len } }

    pub const fn is_fixed(&self) -> bool { self.min == self.max }

    pub const fn check(&self, len: usize) -> bool {
        let len = len as u64;
        len >= self.min && len <= self.max
    }
}

impl Display for Sizing {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match (self.min, self.max) {
            (0, 0xFFFF) => Ok(()),
            (min, max) if min == max => write!(f, " ^ {min}"),
            (0, max) => write!(f, " ^ ..{max:#x}"),
            (min, 0xFFFF) => write!(f, " ^ {min}.."),
            (min, max) => write!(f, " ^ {min}..{max:#x}"),
        }
    }
}

#[derive(Clone, Eq, Debug)]
pub struct Variant {
    pub name: VariantName,
    pub tag: u8,
}
impl_strict_struct!(Variant, STRICT_TYPES_LIB; name, tag);

#[cfg(feature = "serde")]
// The manual serde implementation is needed due to `Variant` bein used as a key in maps (like enum
// or union fields), and serde text implementations such as JSON can't serialize map keys if they
// are not strings. This solves the issue, by putting string serialization of `Variant` for
// human-readable serializers
mod _serde {
    use std::str::FromStr;

    use serde_crate::ser::SerializeStruct;
    use serde_crate::{Deserialize, Deserializer, Serialize, Serializer};

    use super::*;

    impl Serialize for Variant {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where S: Serializer {
            if serializer.is_human_readable() {
                serializer.serialize_str(&format!("{}:{}", self.name, self.tag))
            } else {
                let mut s = serializer.serialize_struct("Variant", 2)?;
                s.serialize_field("name", &self.name)?;
                s.serialize_field("tag", &self.tag)?;
                s.end()
            }
        }
    }

    impl<'de> Deserialize<'de> for Variant {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where D: Deserializer<'de> {
            if deserializer.is_human_readable() {
                let s = String::deserialize(deserializer)?;
                let mut split = s.split(':');
                let (name, tag) = (split.next(), split.next());
                if split.next().is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "Invalid variant format: '{}'. Expected 'name:tag'",
                        s
                    )));
                }
                match (name, tag) {
                    (Some(name), Some(tag)) => {
                        let name = VariantName::from_str(name).map_err(|e| {
                            serde::de::Error::custom(format!("Invalid variant name: {}", e))
                        })?;
                        let tag = tag.parse::<u8>().map_err(|e| {
                            serde::de::Error::custom(format!("Invalid variant tag: {}", e))
                        })?;
                        Ok(Variant { name, tag })
                    }
                    _ => Err(serde::de::Error::custom(format!(
                        "Invalid variant format: '{}'. Expected 'name:tag'",
                        s
                    ))),
                }
            } else {
                #[cfg_attr(
                    feature = "serde",
                    derive(Deserialize),
                    serde(crate = "serde_crate", rename = "Variant")
                )]
                struct VariantFields {
                    name: VariantName,
                    tag: u8,
                }
                let VariantFields { name, tag } = VariantFields::deserialize(deserializer)?;
                Ok(Variant { name, tag })
            }
        }
    }
}

impl Variant {
    pub fn named(tag: u8, name: VariantName) -> Variant { Variant { name, tag } }

    pub fn none() -> Variant {
        Variant {
            name: vname!("none"),
            tag: 0,
        }
    }
    pub fn some() -> Variant {
        Variant {
            name: vname!("some"),
            tag: 1,
        }
    }
}

impl PartialEq for Variant {
    fn eq(&self, other: &Self) -> bool { self.tag == other.tag || self.name == other.name }
}

impl PartialOrd for Variant {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

impl Hash for Variant {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.tag.hash(state);
        self.name.hash(state);
    }
}

impl Ord for Variant {
    fn cmp(&self, other: &Self) -> Ordering {
        if self == other {
            return Ordering::Equal;
        }
        self.tag.cmp(&other.tag)
    }
}

impl Display for Variant {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    #![allow(unused)]

    use std::io::Cursor;

    use crate::*;

    #[cfg(feature = "serde")]
    #[test]
    fn variant_serde_roundtrip() {
        let variant_orig = Variant::strict_dumb();

        // CBOR
        let mut buf = Vec::new();
        ciborium::into_writer(&variant_orig, &mut buf).unwrap();
        let variant_post: Variant = ciborium::from_reader(Cursor::new(&buf)).unwrap();
        assert_eq!(variant_orig, variant_post);

        // JSON
        let variant_str = serde_json::to_string(&variant_orig).unwrap();
        let variant_post: Variant = serde_json::from_str(&variant_str).unwrap();
        assert_eq!(variant_orig, variant_post);

        // YAML
        let variant_str = serde_yaml::to_string(&variant_orig).unwrap();
        let variant_post: Variant = serde_yaml::from_str(&variant_str).unwrap();
        assert_eq!(variant_orig, variant_post);
    }
}
