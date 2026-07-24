# SHA-256 provenance

`sha256.c` and `sha256.h` adapt the public-domain one-shot FIPS-180 SHA-256
implementation shipped by util-linux 2.41.1. The source archive is the
Buildroot-pinned input:

```text
buildroot/dl/util-linux/util-linux-2.41.1.tar.xz
SHA-256 be9ad9a276f4305ab7dd2f5225c8be1ff54352f565ff4dede9628c1aaa7dec57
```

Exact upstream members:

```text
util-linux-2.41.1/lib/sha256.c
SHA-256 188d879bc692d31a64d7b181dacb1099155ffd75fb5c8287e68e4890f67ae047

util-linux-2.41.1/include/sha256.h
SHA-256 96584ed64e789d5c9fd5d447e2623aff0b51d54da1a48b21010e8f4fc60b991a
```

The upstream notice disclaims copyright and places the implementation in the
public domain. `COPYING.sha256` retains that notice. DCENT_OS changes namespace
all symbols, use `size_t`, expose lowercase hex encoding, and apply the project
format. Native tests pin the NIST empty, `abc`, and multi-block vectors plus 12
differential boundary vectors against `sha256sum`. The exact Linaro ARM build is
also required to remain free of a `libcrypto` dependency.
