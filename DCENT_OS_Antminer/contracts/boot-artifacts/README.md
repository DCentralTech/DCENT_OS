# Boot-artifact evidence contract

`scripts/audit_boot_artifacts.py` verifies an explicit, checked-in allowlist of
offline boot artifacts. It is an inventory and static-evidence tool, not a boot
emulator, firmware extractor, hardware probe, or electrical-safety qualifier.

The first catalog is `v1/amlogic.json`. Its scope is deliberately narrow:

- the current tracked Amlogic early-userspace script, whose role is a catalog
  declaration rather than a safety conclusion from this auditor;
- the ignored S19K Pro `a lab unit` raw U-Boot environment and same-capture context;
- ignored S19K and path-declared S21 board-init artifacts;
- one text export declared by its source note to be a transcription; and
- one opaque path-declared file named `uboot.bin`.

Catalog paths, boot phases, product fields, and filename roles are declarations.
Equal bytes are grouped as exact byte strings and never counted as independent
corroboration. Unequal hashes also do not establish independent acquisition,
origin, provenance, or corroboration. In particular, an artifact below an
`s21` directory is not promoted to S21 hardware evidence by its path, a file
named `uboot.bin` is not proven to be executing U-Boot, and the `a lab unit`
environment is not S21 evidence.

## Evidence boundary

The auditor reads each declared file into one immutable byte string, hashes and
parses those same bytes, and never executes, sources, extracts, fixes, fetches,
or rewrites an artifact. It rejects traversal, absolute and non-portable paths,
links and reparse points, non-regular files, hardlinks, oversized inputs,
duplicate JSON keys, case-fold collisions, and detectable replacement while a
file is read.

`catalog_semantic_sha256` identifies the normalized catalog meaning, not the
catalog file's byte representation. Artifact `integrity.sha256` fields identify
the exact artifact bytes read. Projection and context hashes identify only
their explicitly documented derived structures.

Reports contain relative catalog paths, byte counts, hashes, aggregate counts,
and narrowly allowlisted literal `i2c mw` command strings. Arbitrary environment
names and values are reduced to hashes and lengths. Serial numbers, MAC and IP
addresses, credentials, pools, Wi-Fi data, boot arguments, and other captured
values must never be emitted.

For a raw environment, `crc_consistent: true` means only that the bytes match
the catalog-declared four-byte little-endian CRC32 layout. It does not mean
"valid environment." The current `a lab unit` artifact's lexical record shape has one
non-assignment record, and four direct `run` target names referenced by
`preboot` are absent from the captured table. Name presence is not command
resolution. `command_graph_complete` is always false because a lexical scan
cannot evaluate U-Boot hush, compiled defaults, variable expansion,
conditionals, recursive commands, or dynamic `setenv` state. The text-export
decoder reports line shape only; it does not independently prove that the file
is derived or lossy.

No report proves command execution, the active U-Boot adapter, Linux adapter
equivalence, wire framing, acknowledgement, GPIO polarity, APW state, physical
rail state, fan behavior, boot safety, active-copy status, origin authenticity,
or cross-model applicability. `qualification_ready` therefore remains false;
an independent promotion policy would need stronger evidence.

## Presence and exit policy

Tracked artifacts use `required`. Ignored or nonredistributable local evidence
uses `local_optional` so a public clone can run the offline gate honestly.

```sh
python3 scripts/audit_boot_artifacts.py \
  --catalog contracts/boot-artifacts/v1/amlogic.json \
  --project-root . \
  --workspace-root ../.. \
  --json
```

On Windows hosts without a `python3` launcher, use `python` or `py -3` with the
same arguments.

- Exit `0`, `declared_inputs_integrity_and_lexical_checks_passed`: all declared
  entries are present and their applicable checks match.
- Exit `0`, `required_entries_match_declared_local_entries_unavailable`:
  required entries match, but at least one ignored local entry is absent. This
  is not an evidence PASS.
- Exit `1`, `failed`: a required entry is missing, a present entry drifts, an
  input is unsafe/unreadable, decoding fails, or strict local evidence was
  requested but absent.
- Exit `2`: invalid CLI trust roots or catalog/schema errors.

Maintainers closing an archive audit use `--require-local-evidence`. Ordinary
public CI does not require ignored evidence, but it still validates the parser
with hermetic fixtures and verifies every locally present optional artifact.

## Adding evidence

Add entries only after recording exact size, SHA-256, representation, declared
context, provenance grade, association basis, and presence policy. Add a new
decoder profile rather than guessing an unfamiliar CRC layout, redundancy flag,
active copy, padding rule, encryption, or Git-LFS payload state. Unsupported
formats remain opaque.

Do not add recursive workspace discovery. The held firmware corpus contains
duplicates, Git-LFS pointers, encrypted payloads, and conflicting model paths.
Future expansion should add bounded, reviewed catalog entries and representation
profiles for LFS pointers and encrypted recovery artifacts without inferring
hardware identity from filenames.
