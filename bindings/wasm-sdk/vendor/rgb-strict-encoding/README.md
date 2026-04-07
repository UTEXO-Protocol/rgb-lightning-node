# Strict encoding

#### Protobufs for functional programming

This is a set of libraries for deterministic binary serialization using
[strict types] &ndash; type system made with category theory which ensures
provable properties and bounds for the in-memory and serialized type
representation.

## Overview

Strict types is a formal notation for defining and serializing
[generalized algebraic data types (GADT)][gadt] in a deterministic
and confined way. It is developed with [type theory] in mind.

Strict Types are:

* __schema-based__ (with the schema being strict encoding notation),
* __semantic__, i.e. defines types not just as they are layed out in memory,
  but also depending on their meaning,
* __deterministic__, i.e. produces the same result for a given type,
* __portabile__, i.e. can run on ahy hardware architecture and OS, including
  low-performant embedded systems,
* __confined__, i.e. provides guarantees and static analysis on a maximum size
  of the typed data,
* __formally verifiable__.

**Strict Encoding** is set of libraries for serializing / deserializing data
types in binary formats.

![strict-encoding-box](https://user-images.githubusercontent.com/372034/209443924-add45986-d90c-42f9-bfaa-2fd2b0d50506.png)

## Contributing

[CONTRIBUTING.md](../CONTRIBUTING.md)

[strict types]: https://github.com/rgb-protocol/rgb-strict-types

[gadt]: https://en.wikipedia.org/wiki/Algebraic_data_type

[type theory]: https://en.wikipedia.org/wiki/Type_theory
