# transcribe.cpp v0.1.3 crate provenance

Scribe statically links the crates.io packages `transcribe-cpp` and
`transcribe-cpp-sys` at version `0.1.3`, with default features disabled on the
safe wrapper so the release does not opt into shared or accelerator backends.

The exact registry packages are pinned in `Cargo.lock`:

| Package | crates.io checksum |
| --- | --- |
| `transcribe-cpp 0.1.3` | `4c3c4d6136eeccf56cfe8a6669e2d63770d1ef051c7cafd2cb9226218c66cded` |
| `transcribe-cpp-sys 0.1.3` | `278fd6a6da4d9d8d5f2716bd6761a76ea55c129fda6ba57856b80249a8570ed4` |

Both published crates record source commit
`a94e021ef658dc7c788837341a13f6acea3baf3c` in their Cargo VCS metadata and
declare the MIT license. Their identical published `LICENSE` file has SHA-256
`86a53633b56f6b029d3cb42158bcc7aac0cdff898aceb13e83b93e368bbc4ac6`;
that exact notice is preserved beside this file.

Repository: <https://github.com/handy-computer/transcribe.cpp>

The native crate contains the C/C++ implementation compiled into Scribe. It is
a build input, not a separately shipped runtime directory, DLL, or helper
executable.
