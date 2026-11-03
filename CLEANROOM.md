# Clean-room process

Lumen reproduces the observable behaviour of an existing GPL-3.0 project without deriving from its source code. This is what allows Lumen to be released under the permissive MIT OR Apache-2.0 licenses: code derived from GPL or AGPL sources would have to be GPL itself.

## Roles

| Role | Reads upstream C++ | Writes |
|---|---|---|
| Spec writer | Yes | `specs/*.md` only: formats, behaviours, defaults, formulas, in their own words |
| Implementer | **Never** | Rust code under `crates/`, from `specs/` and public documentation only |

## Rules for spec writers

- No copied code, comments, or upstream internal class/function names.
- Allowed: facts imposed by the Wallpaper Engine file formats (magic strings, binary layouts, JSON keys, shader uniform names, enum values, constants, math).
- Ambiguities go in each spec's "Questions ouvertes" section.

## Rules for implementers

- No access to the upstream repository, its forks, or code snippets from it.
- Questions about behaviour go back to a spec writer, who amends the spec; the answer never contains upstream code.
- Each crate's README lists the specs it was built from.

## Record

Every spec change and every crate is committed to git, which serves as the audit trail of who wrote what from which input.
