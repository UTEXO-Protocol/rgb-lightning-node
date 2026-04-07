// Strict encoding library for deterministic binary serialization.
//
// SPDX-License-Identifier: Apache-2.0
//
// Copyright (C) 2025 RGB-Tools developers. All rights reserved.
//
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

use std::io;
use std::num::ParseIntError;
use std::str::FromStr;

use amplify::confinement::{Confined, U32};
use amplify::{Bytes, Bytes32};
pub use bitcoin::constants::ChainHash;
pub use bitcoin::hashes::Hash as _;
pub use bitcoin::key::TweakedPublicKey;
pub use bitcoin::taproot::{LeafScript, LeafVersion};
pub use bitcoin::transaction::Version;
pub use bitcoin::{
    Amount, CompressedPublicKey, OutPoint, PublicKey, ScriptBuf, Sequence, TapNodeHash,
    Transaction, TxIn, TxOut, Txid, Witness, XOnlyPublicKey,
};

use crate::traits::{ReadStruct, ReadTuple, WriteStruct, WriteTuple};
use crate::{
    fname, tn, DecodeError, DefaultBasedStrictDumb, StrictDecode, StrictDeserialize, StrictDumb,
    StrictEncode, StrictProduct, StrictSerialize, StrictStruct, StrictTuple, StrictType, TypeName,
    TypedRead, TypedWrite, LIB_NAME_BITCOIN,
};

impl StrictDumb for Txid {
    fn strict_dumb() -> Self { Txid::all_zeros() }
}

impl StrictProduct for Txid {}

impl StrictType for Txid {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_BITCOIN;
    fn strict_name() -> Option<TypeName> { Some(tn!("Txid")) }
}

impl StrictTuple for Txid {
    const FIELD_COUNT: u8 = 1;
}

impl StrictEncode for Txid {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        let bytes: &[u8; 32] = self.as_ref();
        let confined = Bytes32::from_array(*bytes);
        writer.write_newtype::<Self>(&confined)
    }
}

impl StrictDecode for Txid {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_tuple(|r| {
            let bytes: Bytes32 = r.read_field()?;
            Ok(Txid::from_byte_array(bytes.to_byte_array()))
        })
    }
}

impl StrictSerialize for Txid {}
impl StrictDeserialize for Txid {}

impl StrictDumb for ChainHash {
    fn strict_dumb() -> Self { ChainHash::from([0u8; 32]) }
}

impl StrictProduct for ChainHash {}

impl StrictTuple for ChainHash {
    const FIELD_COUNT: u8 = 1;
}

impl StrictType for ChainHash {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_BITCOIN;
    fn strict_name() -> Option<TypeName> { Some(tn!("ChainHash")) }
}

impl StrictEncode for ChainHash {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        let bytes: [u8; 32] = self.to_bytes();
        let confined = Bytes32::from_array(bytes);
        writer.write_newtype::<Self>(&confined)
    }
}

impl StrictDecode for ChainHash {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_tuple(|r| {
            let bytes: Bytes32 = r.read_field()?;
            Ok(ChainHash::from(bytes.to_byte_array()))
        })
    }
}

impl StrictSerialize for ChainHash {}
impl StrictDeserialize for ChainHash {}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Display, From)]
#[derive(StrictType, StrictDumb, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_BITCOIN, crate = crate)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", transparent)
)]
#[display(inner)]
// 0xFFFFFFFF used in coinbase
pub struct Vout(u32);

impl Vout {
    pub const fn from_u32(u: u32) -> Self { Vout(u) }
    #[inline]
    pub const fn into_u32(self) -> u32 { self.0 }
    #[inline]
    pub const fn into_usize(self) -> usize { self.0 as usize }
    #[inline]
    pub const fn to_u32(&self) -> u32 { self.0 }
    #[inline]
    pub const fn to_usize(&self) -> usize { self.0 as usize }
}

impl FromStr for Vout {
    type Err = ParseIntError;

    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> { s.parse().map(Self) }
}

impl StrictDumb for OutPoint {
    fn strict_dumb() -> Self {
        OutPoint {
            txid: Txid::all_zeros(),
            vout: 0,
        }
    }
}

impl StrictProduct for OutPoint {}

impl StrictType for OutPoint {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_BITCOIN;
    fn strict_name() -> Option<TypeName> { Some(tn!("Outpoint")) }
}

impl StrictStruct for OutPoint {
    const ALL_FIELDS: &'static [&'static str] = &["txid", "vout"];
}

impl StrictEncode for OutPoint {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        writer.write_struct::<Self>(|w| {
            Ok(w.write_field(fname!("txid"), &self.txid)?
                .write_field(fname!("vout"), &Vout::from_u32(self.vout))?
                .complete())
        })
    }
}

impl StrictDecode for OutPoint {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_struct(|reader| {
            let txid = reader.read_field(fname!("txid"))?;
            let vout = reader.read_field(fname!("vout"))?;
            Ok(OutPoint { txid, vout })
        })
    }
}

impl StrictSerialize for OutPoint {}
impl StrictDeserialize for OutPoint {}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
#[derive(StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_BITCOIN, crate = crate)]
pub struct TxVer(pub i32);

impl Default for TxVer {
    fn default() -> Self { TxVer(2) }
}

impl DefaultBasedStrictDumb for TxVer {}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default)]
#[derive(StrictType, StrictEncode, StrictDecode, StrictDumb)]
#[strict_type(lib = LIB_NAME_BITCOIN, crate = crate)]
pub struct LockTime(pub u32);

impl StrictDumb for Transaction {
    fn strict_dumb() -> Self {
        Transaction {
            version: Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: Vec::new(),
            output: Vec::new(),
        }
    }
}

impl StrictProduct for Transaction {}

impl StrictStruct for Transaction {
    const ALL_FIELDS: &'static [&'static str] = &["version", "inputs", "outputs", "lockTime"];
}

impl StrictType for Transaction {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_BITCOIN;
    fn strict_name() -> Option<TypeName> { Some(tn!("Tx")) }
}

impl StrictEncode for Transaction {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        writer.write_struct::<Self>(|w| {
            let input_confined = Confined::<Vec<TxIn>, 0, U32>::try_from(self.input.clone())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "too many inputs"))?;
            let output_confined = Confined::<Vec<TxOut>, 0, U32>::try_from(self.output.clone())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "too many outputs"))?;
            Ok(w.write_field(fname!("version"), &TxVer(self.version.0))?
                .write_field(fname!("inputs"), &input_confined)?
                .write_field(fname!("outputs"), &output_confined)?
                .write_field(fname!("lockTime"), &LockTime(self.lock_time.to_consensus_u32()))?
                .complete())
        })
    }
}

impl StrictDecode for Transaction {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_struct(|reader| {
            let version: i32 = reader.read_field(fname!("version"))?;
            let input_confined: Confined<Vec<TxIn>, 0, U32> =
                reader.read_field(fname!("inputs"))?;
            let output_confined: Confined<Vec<TxOut>, 0, U32> =
                reader.read_field(fname!("outputs"))?;
            let lock_time: u32 = reader.read_field(fname!("lockTime"))?;
            Ok(Transaction {
                version: Version(version),
                lock_time: bitcoin::absolute::LockTime::from_consensus(lock_time),
                input: input_confined.release(),
                output: output_confined.release(),
            })
        })
    }
}

impl StrictSerialize for Transaction {}
impl StrictDeserialize for Transaction {}

impl StrictDumb for Witness {
    fn strict_dumb() -> Self { Witness::new() }
}

impl StrictProduct for Witness {}

impl StrictTuple for Witness {
    const FIELD_COUNT: u8 = 1;
}

impl StrictType for Witness {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_BITCOIN;
    fn strict_name() -> Option<TypeName> { Some(tn!("Witness")) }
}

impl StrictEncode for Witness {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        let byte_strs: Vec<ByteStr> =
            self.iter().map(ByteStr::from_slice).collect::<Result<Vec<_>, _>>()?;
        let confined_array = Confined::<Vec<ByteStr>, 0, U32>::try_from(byte_strs)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "too many witness items"))?;
        writer.write_newtype::<Self>(&confined_array)
    }
}

impl StrictDecode for Witness {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_tuple(|reader| {
            let confined_array: Confined<Vec<ByteStr>, 0, U32> = reader.read_field()?;
            let byte_strs = confined_array.release();
            let witness_items: Vec<Vec<u8>> =
                byte_strs.into_iter().map(|byte_str| byte_str.as_ref().to_vec()).collect();
            Ok(Witness::from_slice(&witness_items))
        })
    }
}

impl StrictSerialize for Witness {}
impl StrictDeserialize for Witness {}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[derive(StrictType, StrictDumb, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_BITCOIN, crate = crate)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", transparent)
)]
pub struct SeqNo(pub u32);

impl StrictDumb for TxIn {
    fn strict_dumb() -> Self {
        TxIn {
            previous_output: OutPoint::strict_dumb(),
            script_sig: ScriptBuf::strict_dumb(),
            sequence: Sequence::ZERO,
            witness: Witness::strict_dumb(),
        }
    }
}

impl StrictProduct for TxIn {}

impl StrictType for TxIn {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_BITCOIN;
    fn strict_name() -> Option<TypeName> { Some(tn!("TxIn")) }
}

impl StrictStruct for TxIn {
    const ALL_FIELDS: &'static [&'static str] = &["prevOutput", "sigScript", "sequence", "witness"];
}

impl StrictEncode for TxIn {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        writer.write_struct::<Self>(|w| {
            let script_sig = ScriptBytes::try_from(self.script_sig.to_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "script sig too large"))?;
            Ok(w.write_field(fname!("prevOutput"), &self.previous_output)?
                .write_field(fname!("sigScript"), &SigScript(script_sig))?
                .write_field(fname!("sequence"), &SeqNo(self.sequence.to_consensus_u32()))?
                .write_field(fname!("witness"), &self.witness)?
                .complete())
        })
    }
}

impl StrictDecode for TxIn {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_struct(|reader| {
            let previous_output: OutPoint = reader.read_field(fname!("prevOutput"))?;
            let script_sig: ScriptBuf = reader.read_field(fname!("sigScript"))?;
            let sequence: u32 = reader.read_field(fname!("sequence"))?;
            let witness: Witness = reader.read_field(fname!("witness"))?;
            let sequence = Sequence::from_consensus(sequence);
            Ok(TxIn {
                previous_output,
                script_sig,
                sequence,
                witness,
            })
        })
    }
}

impl StrictSerialize for TxIn {}
impl StrictDeserialize for TxIn {}

#[derive(
    Wrapper, WrapperMut, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, From, Default
)]
#[wrapper(Add, Sub, Mul, Div, FromStr)]
#[wrapper_mut(MathAssign)]
#[derive(StrictType, StrictDumb, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_BITCOIN, crate = crate)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", transparent)
)]
pub struct Sats(
    #[from]
    #[from(u32)]
    #[from(u16)]
    #[from(u8)]
    pub u64,
);

impl Sats {
    pub const ZERO: Self = Sats(0);
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default)]
#[derive(StrictType, StrictDumb, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_BITCOIN, crate = crate)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", transparent)
)]
pub struct ScriptBytes(pub Confined<Vec<u8>, 0, U32>);

impl TryFrom<Vec<u8>> for ScriptBytes {
    type Error = amplify::confinement::Error;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        Confined::try_from(bytes).map(ScriptBytes)
    }
}

impl TryFrom<&[u8]> for ScriptBytes {
    type Error = amplify::confinement::Error;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> { Self::try_from(bytes.to_vec()) }
}

impl AsRef<[u8]> for ScriptBytes {
    fn as_ref(&self) -> &[u8] { self.0.as_slice() }
}

impl ScriptBytes {
    pub fn into_inner(self) -> Vec<u8> { self.0.release() }
}

#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, From, Default)]
#[derive(StrictType, StrictDumb, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_BITCOIN, crate = crate)]
pub struct SigScript(ScriptBytes);

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[derive(StrictType, StrictDumb, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_BITCOIN, crate = crate)]
pub struct ByteStr(pub Confined<Vec<u8>, 0, U32>);

impl Default for ByteStr {
    fn default() -> Self { ByteStr(Confined::try_from(Vec::new()).unwrap()) }
}

impl ByteStr {
    pub fn new() -> Self { Self::default() }

    pub fn from_slice(data: &[u8]) -> Result<Self, io::Error> {
        Self::try_from(data)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "data too large for ByteStr"))
    }

    pub fn len(&self) -> usize { self.0.len() }

    pub fn is_empty(&self) -> bool { self.0.is_empty() }
}

impl TryFrom<Vec<u8>> for ByteStr {
    type Error = amplify::confinement::Error;

    fn try_from(data: Vec<u8>) -> Result<Self, Self::Error> {
        Confined::try_from(data).map(ByteStr)
    }
}

impl TryFrom<&[u8]> for ByteStr {
    type Error = amplify::confinement::Error;

    fn try_from(data: &[u8]) -> Result<Self, Self::Error> { Self::try_from(data.to_vec()) }
}

impl AsRef<[u8]> for ByteStr {
    fn as_ref(&self) -> &[u8] { &self.0 }
}

impl StrictDumb for TxOut {
    fn strict_dumb() -> Self {
        TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::new(),
        }
    }
}

impl StrictProduct for TxOut {}

impl StrictStruct for TxOut {
    const ALL_FIELDS: &'static [&'static str] = &["value", "scriptPubkey"];
}

impl StrictType for TxOut {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_BITCOIN;
    fn strict_name() -> Option<TypeName> { Some(tn!("TxOut")) }
}

impl StrictEncode for TxOut {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        writer.write_struct::<Self>(|w| {
            Ok(w.write_field(fname!("value"), &Sats(self.value.to_sat()))?
                .write_field(fname!("scriptPubkey"), &self.script_pubkey)?
                .complete())
        })
    }
}

impl StrictDecode for TxOut {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_struct(|reader| {
            let value: u64 = reader.read_field(fname!("value"))?;
            let script_pubkey: ScriptBuf = reader.read_field(fname!("scriptPubkey"))?;
            let value = Amount::from_sat(value);
            Ok(TxOut {
                value,
                script_pubkey,
            })
        })
    }
}

impl StrictSerialize for TxOut {}
impl StrictDeserialize for TxOut {}

impl StrictDumb for ScriptBuf {
    fn strict_dumb() -> Self { ScriptBuf::new() }
}

impl StrictProduct for ScriptBuf {}

impl StrictTuple for ScriptBuf {
    const FIELD_COUNT: u8 = 1;
}

impl StrictType for ScriptBuf {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_BITCOIN;
    fn strict_name() -> Option<TypeName> { Some(tn!("ScriptPubkey")) }
}

impl StrictEncode for ScriptBuf {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        let bytes = self.as_bytes().to_vec();
        let script_bytes = ScriptBytes::try_from(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "script too large"))?;
        writer.write_newtype::<Self>(&script_bytes)
    }
}

impl StrictDecode for ScriptBuf {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_tuple(|reader| {
            let script_bytes: ScriptBytes = reader.read_field()?;
            Ok(ScriptBuf::from_bytes(script_bytes.into_inner()))
        })
    }
}

impl StrictSerialize for ScriptBuf {}
impl StrictDeserialize for ScriptBuf {}

impl StrictDumb for TapNodeHash {
    fn strict_dumb() -> Self { TapNodeHash::all_zeros() }
}

impl StrictProduct for TapNodeHash {}

impl StrictTuple for TapNodeHash {
    const FIELD_COUNT: u8 = 1;
}

impl StrictType for TapNodeHash {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_BITCOIN;
    fn strict_name() -> Option<TypeName> { Some(tn!("TapNodeHash")) }
}

impl StrictEncode for TapNodeHash {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        let bytes: [u8; 32] = self.to_byte_array();
        let confined = Bytes32::from_array(bytes);
        writer.write_newtype::<Self>(&confined)
    }
}

impl StrictDecode for TapNodeHash {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_tuple(|r| {
            let bytes: Bytes32 = r.read_field()?;
            Ok(TapNodeHash::from_byte_array(bytes.to_byte_array()))
        })
    }
}

impl StrictSerialize for TapNodeHash {}
impl StrictDeserialize for TapNodeHash {}

impl StrictDumb for LeafVersion {
    fn strict_dumb() -> Self { LeafVersion::TapScript }
}

impl StrictProduct for LeafVersion {}

impl StrictTuple for LeafVersion {
    const FIELD_COUNT: u8 = 1;
}

impl StrictType for LeafVersion {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_BITCOIN;
    fn strict_name() -> Option<TypeName> { Some(tn!("LeafVer")) }
}

impl StrictEncode for LeafVersion {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        writer.write_tuple::<Self>(|w| Ok(w.write_field(&self.to_consensus())?.complete()))
    }
}

impl StrictDecode for LeafVersion {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_tuple(|r| {
            let version_byte: u8 = r.read_field()?;
            LeafVersion::from_consensus(version_byte).map_err(|err| {
                DecodeError::DataIntegrityError(format!("invalid leaf version: {}", err))
            })
        })
    }
}

impl StrictSerialize for LeafVersion {}
impl StrictDeserialize for LeafVersion {}

impl StrictDumb for LeafScript<ScriptBuf> {
    fn strict_dumb() -> Self {
        LeafScript {
            version: LeafVersion::TapScript,
            script: ScriptBuf::new(),
        }
    }
}

impl StrictProduct for LeafScript<ScriptBuf> {}

impl StrictType for LeafScript<ScriptBuf> {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_BITCOIN;
    fn strict_name() -> Option<TypeName> { Some(tn!("LeafScript")) }
}

impl StrictStruct for LeafScript<ScriptBuf> {
    const ALL_FIELDS: &'static [&'static str] = &["version", "script"];
}

impl StrictEncode for LeafScript<ScriptBuf> {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        writer.write_struct::<Self>(|w| {
            let script_bytes = ScriptBytes::try_from(self.script.to_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "script too large"))?;
            Ok(w.write_field(fname!("version"), &self.version)?
                .write_field(fname!("script"), &script_bytes)?
                .complete())
        })
    }
}

impl StrictDecode for LeafScript<ScriptBuf> {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_struct(|reader| {
            let version = reader.read_field(fname!("version"))?;
            let script = reader.read_field(fname!("script"))?;
            Ok(Self { version, script })
        })
    }
}

impl StrictSerialize for LeafScript<ScriptBuf> {}
impl StrictDeserialize for LeafScript<ScriptBuf> {}

impl StrictDumb for XOnlyPublicKey {
    fn strict_dumb() -> Self { XOnlyPublicKey::from_slice(&[1u8; 32]).unwrap() }
}

impl StrictProduct for XOnlyPublicKey {}

impl StrictType for XOnlyPublicKey {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_BITCOIN;
    fn strict_name() -> Option<TypeName> { Some(tn!("XOnlyPk")) }
}

impl StrictTuple for XOnlyPublicKey {
    const FIELD_COUNT: u8 = 1;
}

impl StrictEncode for XOnlyPublicKey {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        let bytes: [u8; 32] = self.serialize();
        let confined = Bytes32::from_array(bytes);
        writer.write_newtype::<Self>(&confined)
    }
}

impl StrictDecode for XOnlyPublicKey {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_tuple(|r| {
            let bytes: Bytes32 = r.read_field()?;
            XOnlyPublicKey::from_slice(bytes.as_slice()).map_err(|err| {
                DecodeError::DataIntegrityError(format!("invalid x-only public key: {}", err))
            })
        })
    }
}

impl StrictSerialize for XOnlyPublicKey {}
impl StrictDeserialize for XOnlyPublicKey {}

impl StrictDumb for TweakedPublicKey {
    fn strict_dumb() -> Self {
        TweakedPublicKey::dangerous_assume_tweaked(XOnlyPublicKey::from_slice(&[1u8; 32]).unwrap())
    }
}

impl StrictProduct for TweakedPublicKey {}

impl StrictType for TweakedPublicKey {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_BITCOIN;
    fn strict_name() -> Option<TypeName> { Some(tn!("OutputPk")) }
}

impl StrictTuple for TweakedPublicKey {
    const FIELD_COUNT: u8 = 1;
}

impl StrictEncode for TweakedPublicKey {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        let bytes: [u8; 32] = self.serialize();
        let xonly = XOnlyPublicKey::from_slice(&bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid x-only public key"))?;
        writer.write_newtype::<Self>(&xonly)
    }
}

impl StrictDecode for TweakedPublicKey {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_tuple(|r| {
            let xonly: XOnlyPublicKey = r.read_field()?;
            Ok(TweakedPublicKey::dangerous_assume_tweaked(xonly))
        })
    }
}

impl StrictSerialize for TweakedPublicKey {}
impl StrictDeserialize for TweakedPublicKey {}

impl StrictDumb for CompressedPublicKey {
    fn strict_dumb() -> Self { CompressedPublicKey::from_slice(&[2u8; 33]).unwrap() }
}

impl StrictProduct for CompressedPublicKey {}

impl StrictType for CompressedPublicKey {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_BITCOIN;
    fn strict_name() -> Option<TypeName> { Some(tn!("CompressedPk")) }
}

impl StrictTuple for CompressedPublicKey {
    const FIELD_COUNT: u8 = 1;
}

impl StrictEncode for CompressedPublicKey {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        let bytes: [u8; 33] = self.to_bytes();
        let confined = Bytes::<33>::from_array(bytes);
        writer.write_newtype::<Self>(&confined)
    }
}

impl StrictDecode for CompressedPublicKey {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_tuple(|r| {
            let bytes: Bytes<33> = r.read_field()?;
            CompressedPublicKey::from_slice(bytes.as_slice()).map_err(|err| {
                DecodeError::DataIntegrityError(format!("invalid compressed public key: {}", err))
            })
        })
    }
}

impl StrictSerialize for CompressedPublicKey {}
impl StrictDeserialize for CompressedPublicKey {}
