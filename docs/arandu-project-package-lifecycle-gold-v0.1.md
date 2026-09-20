# Arandu — Project & Package Lifecycle Gold v0.1

> **Registro de decisão concluído.** O planejamento desta campanha foi
> consolidado em “Decisões consolidadas (Gold v0.1)” no roadmap mestre. Este
> arquivo agora serve apenas como contrato detalhado e evidência técnica.

**Status:** implemented contract; Gold scope complete
**Owner:** `arandu_cli` orchestrates filesystem/network effects;
`arandu_query` owns deterministic manifest, package-graph and directory inputs.  
**Prerequisite:** compiler, distribution and installed-SDK gates are Gold in their
published scope.

## Visão Geral e Contexto

Turn the existing project-mode CLI into a reproducible package lifecycle that
works outside the monorepo on Windows, Linux and macOS. A user must be able to
create, inspect, build, test and clean a project without knowing compiler
internals. The result is the prerequisite for the testing/benchmark harness,
the remote compiler service and the public playground.

Gold here does **not** mean a public registry. The first stable package graph is
local/workspace-first. Remote Git and registry resolution remain disabled until
identity, lockfile integrity and cache isolation are proven.

## Detalhes Técnicos da Implementação

| Area | Current state | Gold gap |
| --- | --- | --- |
| Project creation | transactional `new`/`init`, bin/lib targets, README, ignore file and VCS policy | lifecycle E2E remains in P7 |
| Manifest | strict deterministic TOML with schema, edition, targets, path dependencies, exports, workspace members and compatibility validation | registry/Git origins and target-specific dependency features remain future work |
| Incrementality | manifest fields and an authoritative `PackageModuleMap` are stable Salsa inputs, including live background LSP reload | remote resolver/cache inputs begin in P5/P6 |
| Build | Cranelift emits a baseline dev object or speed-oriented release object, links the packaged runtime and atomically publishes a content-addressed executable in its profile | cross-target builds and LLVM/LTO/PGO remain out of scope |
| Dependencies | deterministic local workspace DAG, direct aliases, explicit exports and a semantic lockfile | verified global cache and remote resolver start in P5/P6 |
| Imports | `self`, `std` and direct dependency roots are logical; private, transitive and quoted filesystem access fail closed | complete live CLI/LSP graph refresh and edition removal of the temporary bare-local migration |
| Portability | installed SDK smoke is native on three OS families | generated projects are not yet exercised as a complete lifecycle outside checkout |

The existing narrow query boundaries are retained. Parsing a manifest is pure;
reading it, walking directories, resolving VCS and writing files stay in CLI or
dedicated effectful infrastructure before values enter Salsa.

### Decisions informed by existing ecosystems

### Why TOML remains the right manifest format

The comparison was reopened rather than inherited from Cargo:

| Format | Strength | Reason not selected |
| --- | --- | --- |
| JSON | ubiquitous, strict data model and strong schema tooling | no native comments; noisy for hand-maintained dependency tables |
| YAML | concise and expressive | specification and implicit typing are too broad for a compiler control file; parser behavior is harder to constrain |
| custom `mod`-style DSL | could optimize package syntax | creates a second language, parser, formatter and editor ecosystem with no semantic benefit yet |
| executable manifest | arbitrary build logic | reading dependencies becomes host-code execution and harms reproducibility/security |
| TOML 1.0 | typed, UTF-8, comments, unambiguous tables, mature Rust parsers | table/dotted-key rules require good diagnostics and tool edits must preserve user formatting |

**Decision:** both `arandu.toml` and generated `arandu.lock` use TOML 1.0.
The manifest accepts the complete TOML syntax but a strict Arandu schema. The
lockfile uses a canonical machine-written TOML subset so byte determinism is
under our control. We do not maintain a partial TOML parser.

`arandu check/build/run` never rewrite the user manifest. Future `add/remove`
commands must use a lossless TOML document editor or narrowly edit the owned
dependency table; deserializing and serializing the entire file is forbidden
because it would erase comments and create noisy diffs.

This choice follows the same human-editable/static-metadata rationale recorded
by Python packaging, while avoiding Python's later ambiguity between static and
dynamic metadata: Arandu metadata is static unless a future field explicitly
defines a reproducible source. No build backend may silently replace a value
written by the user.

### Declarative, versioned manifest

The canonical filename is **`arandu.toml`** and the generated lockfile is
**`arandu.lock`**. Lowercase avoids case-only ambiguity across filesystems and
matches the spelling used in commands and documentation. Migration from the
legacy `Arandu.toml` is explicit:

1. discovery prefers `arandu.toml`;
2. a legacy-only project loads with one structured migration warning;
3. if two distinct files exist, commands fail instead of guessing;
4. `arandu init/new` only write the canonical name;
5. legacy discovery is removed only at a declared release boundary.

The manifest is data, never executable code. Unlike programmable build
manifests, loading an untrusted package cannot run arbitrary host code. Unknown
keys in Arandu-owned tables are errors with suggestions; a namespaced
`[metadata]` table is reserved for forward-compatible third-party data.

Initial schema:

```toml
schema = 1

[package]
name = "hello"
version = "0.1.0"
edition = "2026"

[toolchain]
arandu = ">=0.1.0-rc.4, <0.2.0"

[targets.bin]
name = "hello"
root = "src/main.aru"

[dependencies]
# util = { path = "../util" }

[capabilities]
network = []
filesystem_read = ["assets/**"]
filesystem_write = []
environment = []
process = []
foreign = false

[policy.effects]
deny_new_authority = true
warn_new_resources = true
deny = ["UnknownCapability"]
```

These fields are a versioned policy reservation, not a claim that A2 inference
already exists. Empty capability lists deny authority. A future compiler
summary is generated from typed code and compared against this ceiling; authors
cannot make unsafe behavior safe merely by editing the manifest.

Paths are UTF-8 manifest data resolved relative to the manifest directory,
lexically normalized and then containment-checked where containment is part of
the contract. Package names reject Windows device names and other spellings
that cannot round-trip on supported filesystems.

### Lockfile is resolution, not a cache

`arandu.lock` is generated by the tool, deterministic and committed for root
applications and workspaces. It contains a format version, package identity,
exact source, exact revision/version, content digest and dependency edges. It
must not contain absolute local paths, timestamps, host separators or map
iteration order.

- normal local commands may create/update it only when resolution is required;
- `--locked` fails if it is missing or would change;
- `--offline` forbids all network access but may resolve from verified cache;
- `--frozen` means `--locked --offline`;
- writes use temporary sibling + flush + atomic replace;
- parse or integrity failure never falls back to an unlocked build.

P3 implements this as a strict typed model rather than a generic preserved
metadata map. The manifest fingerprint hashes a length-delimited semantic
projection, so comments and TOML layout do not create lock churn while every
resolution input does. Version 1 admits only `root` and normalized
workspace-relative `path+...` sources; registry, Git and other origins remain
unreadable until their typed contracts exist. This fail-closed boundary prevents
an old client from silently interpreting a future origin incorrectly.

The implementation deliberately avoids two ecosystem traps. Cargo's own
format notes discourage opaque metadata that old clients merely preserve, so
unknown Arandu-owned fields and future versions are errors. npm documents that
its hidden lock can trust directory modification times even though nested
edits may not update them; Arandu validates canonical content and fingerprints
instead of timestamps. Existing lock corruption is never overwritten as an
ordinary update: the user must inspect or deliberately remove it.

Path dependencies participate by canonical package identity and manifest
fingerprint but are not copied into the global cache. The lockfile records a
portable relative path only when the dependency is inside the workspace.

### Three storage domains

Do not overload one `target` directory with unrelated trust and lifetime rules:

| Domain | Location | Contract |
| --- | --- | --- |
| project artifacts | `<workspace>/target/` | disposable; ignored by Git; `arandu clean` owns only this verified directory |
| project metadata | `<workspace>/.arandu/` | disposable resolution/query metadata; no downloaded executable code |
| global package cache | platform cache directory | content-addressed immutable sources; shared safely; explicit verify/prune commands |

Project output is separated by profile and target triple, strictly limited to final deliverables ([RFC 0019](./rfcs/0019-zero-bloat-target-and-shared-cache.md)):

```text
target/<profile>/<target-triple>/bin/
target/<profile>/<target-triple>/lib/
target/<profile>/<target-triple>/build-state.json
```

All intermediate artifacts (relocatable objects, CGUs, incremental dependency graphs and cached `.amir`/`.air` envelopes) reside in the global Content-Addressable Storage (CAS) under the platform cache directory (`~/.cache/arandu/cas/`), subject to automatic LRU disk budget enforcement (default: 2.0 GiB) to eliminate disk bloat across projects.

Final artifacts are published from CAS to the project `target/` using copy-on-write reflinks (`ioctl(FICLONE)` on Linux, `clonefile` on macOS APFS, hardlinks or atomic staging plus rename as fallback). A failed build cannot replace the last valid binary. `clean` resolves and validates the exact project root and refuses symlink/junction escapes or broad targets.

`build` records schema-2 provenance. The linked executable is immutable under `bin/`; and `build-state.json` selects the current successful artifact. Unix replaces this state with atomic `rename`; Windows uses `ReplaceFileW` with write-through. Link or compilation failure occurs before that commit, so the previous state
and executable remain runnable. The SDK ships its target-matched
`arandu_runtime` static library; its C ABI is compiler-internal, not public FFI.

### Package identity and resolution

Identity is not a display name alone. The internal key is `(canonical source,
package name)`; a resolved node adds exact version/revision and content digest.
This prevents registry/Git/path aliases from silently becoming the same or two
spellings of the same source from entering the graph twice.

Resolution v0.1 is deliberately small:

1. workspace members;
2. relative path dependencies;
3. exact Git revisions only after the local graph is Gold;
4. registry and broad semantic-version solving later.

The graph is sorted before diagnostics or serialization. Cycles, duplicate
identities, missing manifests, root escapes and source collisions are hard
errors. Dependency manifests cannot select root profiles or mutate the root
workspace.

### Package, target and module are different identities

Arandu adopts four explicit layers instead of treating a source path as all of
them at once:

| Layer | Meaning | Example |
| --- | --- | --- |
| package | versioned/distributed unit from one manifest | `acme_math@1.2.0` |
| target | independently compiled product inside a package | library `math`, binary `calc`, tests |
| module | namespace/analysis unit inside one target | `self.geometry.vector` |
| file | current physical source of a module | `src/geometry/vector.aru` |

`PackageId`, `TargetId` and `ModuleId` must be typed identities. None is a raw
path or a `SymbolId`. A file rename may change the module mapping, but package
resolution never manufactures a `FileId`; CLI/LSP register sources and provide
the resulting immutable package/module map as Salsa inputs.

The manifest target shape replaces the MVP `kind`/`entry` pair before it becomes
a compatibility burden:

```toml
schema = 1

[package]
name = "calculator"
version = "0.1.0"
edition = "2026"

[targets.lib]
name = "calculator"
root = "src/lib.aru"

[targets.bin]
name = "calc"
root = "src/main.aru"

[dependencies]
math = { path = "../math" }
```

The dependency table key (`math`) is the **source-level binding**, while the
resolved package keeps its own declared name and canonical source identity.
Consumers therefore survive an upstream repository rename and two packages
with similar display names can be bound under distinct aliases. Two aliases
that resolve to the same package identity are rejected initially rather than
compiling the same package twice under ambiguous namespaces.

This deliberately combines the strongest parts of Go and Deno without making
source imports a transport protocol. Like Go, a package path is resolved from
an already selected module graph. Like Deno import maps, a short source-level
name is bound centrally. Unlike Go's missing-package lookup and Deno's direct
URL specifiers, an Arandu `import` never searches a registry, contacts a proxy,
or embeds a version/URL. Only an explicit manifest dependency can introduce an
external package; `check`, `build` and `run` do not mutate that graph.

### Canonical import roots

Package mode has three unambiguous roots:

```text
std.path                    standard library
self.geometry.vector        another module in the current target
math.geometry.vector        exported module from direct dependency alias `math`
```

Existing source forms remain useful:

```aru
import self.geometry.vector as vector
import math.geometry as geometry
from math.geometry import { Point, distance }
```

Rules:

1. only a manifest-declared **direct** dependency alias may occupy the first
   external segment; transitive dependencies never leak into source lookup;
2. `self` never means a filesystem current directory and cannot escape its
   target;
3. `std` is a reserved toolchain root and cannot be shadowed by a package;
4. source imports never contain versions, URLs, Git revisions or cache paths;
5. dotted paths are case-sensitive logical identifiers but creation rejects
   case-fold collisions that would break Windows/macOS checkouts;
6. extensionless dotted syntax has exactly one resolution; no probing
   `foo.aru` versus `foo/index.aru` or several source roots;
7. import aliases are file-scoped, matching the current resolver and avoiding
   one file silently changing another file's namespace;
8. package dependency cycles are rejected. Existing deterministic module-cycle
   recovery remains a compiler diagnostic, not permission for cyclic packages.

The current bare form (`import util`) is ambiguous between a local module and a
future dependency named `util`. In package mode it migrates deterministically:

- if `util` is a declared direct dependency alias, it means that dependency's
  root export;
- otherwise the legacy local interpretation is accepted with a structured fix
  to `import self.util` during the migration edition;
- a later edition removes implicit local roots.

Quoted imports such as `import "vendor/file.aru" as vendor` are not a package
dependency mechanism. In package mode, arbitrary quoted filesystem sources are
rejected unless a future explicit foreign-source manifest contract authorizes
them. Single-file CLI mode may retain them for compatibility without allowing
them to enter a resolved package graph.

### Public module surface, no deep imports

An external package cannot import every `.aru` file merely because it exists.
The library target declares an export map; everything else is package-internal:

```toml
[targets.lib]
name = "math"
root = "src/lib.aru"

[targets.lib.exports]
"." = "src/lib.aru"
"geometry" = "src/geometry.aru"
"geometry.vector" = "src/geometry/vector.aru"
```

Thus `import math.geometry` is stable API while
`import math.internal.fast_inverse_sqrt` fails even if that file is present.
The public path is decoupled from layout, preventing consumers from freezing a
dependency's private directory structure. Export targets must be unique,
inside the package, part of the declared library target and case-fold unique.

Inside an exported module, only existing `public` declarations cross the
module boundary. Both gates are required:

```text
package export map permits module
            AND
exported_symbols permits declaration
```

This preserves the current `exported_symbols` early cutoff: editing a private
body does not invalidate dependants, changing the export map invalidates only
the affected package surface, and changing a public signature invalidates its
importers. A future source-level re-export can add convenience, but it must feed
the same explicit export surface rather than creating a second resolver.

### Package graph and module graph stay separate

Resolution occurs in two pure deterministic stages after effectful discovery:

1. **Package graph:** manifest requirements → exact `PackageId`s, targets and
   direct dependency aliases; serialized in `arandu.lock`.
2. **Module graph:** source import paths → `ModuleId`s using the already resolved
   target/module maps; tracked by Salsa and never performs network or filesystem
   discovery.

The lockfile records package edges, not every source import. Module dependencies
remain compiler analysis data and may change on each edit without rewriting the
lockfile. Conversely, changing a dependency alias or resolved package version
replaces the relevant module root as one narrow Salsa input.

`ModuleRoots` therefore evolves from one package plus stdlib into an immutable
`PackageModuleMap` containing:

- current package and target;
- `self` module mapping;
- direct dependency alias → exported module mapping;
- stdlib mapping;
- one deterministic reverse map for diagnostics/LSP.

`canonicalize_import_path` may continue to normalize syntax, but it must return
a logical import (`Std`, `SelfModule`, `DependencyModule`, `LegacyExternal`),
not a guessed filesystem string. Filesystem paths are resolved before query
execution and installed through inputs.

Module/package regressions required before Gold:

- dependency alias root and exported subpath resolve in CLI and LSP;
- undeclared transitive dependency and non-exported deep module fail;
- `self`, dependency and `std` cannot shadow one another;
- two files/exports differing only by case fail portably;
- module/package cycles and duplicate source identities are deterministic;
- path dependency rename/removal refreshes completion, goto and diagnostics;
- dependency body-only edit preserves importer cutoff when exports are stable;
- public signature or export-map edit invalidates exactly the affected importers;
- a malicious module path cannot escape source/cache/workspace roots;
- Windows separators, symlinks/junctions and Unicode normalize to one identity.

### No dependency code execution during resolution

Arandu v0.1 has no install hooks, lifecycle scripts or executable build
manifests. Fetching/resolving a dependency performs data parsing and content
verification only. Future build extensions require a separate RFC with an
explicit capability sandbox; they cannot arrive as an incidental manifest
field.

### Supply-chain threat model

A checksum proves that downloaded bytes equal expected bytes. It does **not**
prove that those bytes are safe: a malicious maintainer can publish malicious
source with a perfectly valid checksum, signature and provenance statement.
Arandu therefore separates four claims:

| Claim | Mechanism | What it does not prove |
| --- | --- | --- |
| integrity | content digest in lockfile/cache | author intent or code safety |
| origin authenticity | canonical source plus signed registry metadata | that a legitimate publisher is uncompromised |
| build provenance | signed source/build attestation and transparency record | absence of malicious source/build instructions |
| policy/audit | explicit review, advisories, source/capability policy | mathematical safety of dependency behavior |

#### Closed-world resolution

- An import never searches a public registry. Only dependencies already named
  in `arandu.toml` enter resolution; missing aliases are errors with no network.
- Every dependency has exactly one explicit source class (`path`, `git`, later
  `registry`) and canonical origin. A private/Git lookup never falls back to a
  public package with the same name.
- The lockfile records the complete transitive graph, canonical origin, exact
  revision/version and normalized source-tree digest. A mirror may replace
  transport only when it serves identical authenticated content.
- CI and release builds use `--frozen`; manifest/lock mismatch, absent cache or
  any attempted network access fails closed.
- Resolver limits bound graph nodes, depth, manifest/archive size and expanded
  file count before untrusted input can exhaust memory or disk.

This blocks dependency-confusion auto-resolution, tag retargeting after lock,
MITM/cache substitution and silent transitive drift. It deliberately gives up
the convenience of discovering a dependency from a source import.

#### Deliberate trust on first addition

The first `arandu add` is the dangerous moment. It must be an explicit command,
never a side effect of `check/build/run`, and must display before mutation:

```text
source identity
selected immutable revision/version
publisher/provenance status when available
new direct and transitive packages
license/advisory status when available
whether any native/binary capability is requested
```

`--yes` is forbidden when trust-relevant information changed unless CI supplies
an explicit policy file. Manifest and lockfile update atomically together only
after resolution succeeds. `arandu update` defaults to a plan/diff and one
named dependency; bulk/latest updates require an explicit flag. Ordinary builds
never opportunistically select a newer version.

For exact Git dependencies, the user specifies a canonical HTTPS/SSH origin and
full commit ID. Arandu hashes the normalized source tree and records both commit
and digest: Git identity aids audit, while the independent digest detects a
different archive/tree. Floating branch/tag requirements are excluded from the
first remote slice.

#### Registry requirements before a registry exists

The first Gold campaign does not launch a registry. A future Arandu registry
must satisfy a separate security gate before `registry = ...` is accepted:

- scoped package identity bound to a publisher/organization, with protected
  names and no public fallback for configured private scopes;
- immutable `(identity, version) → content digest`; yanks hide selection but do
  not delete bytes required by old lockfiles;
- signed, versioned and expiring root/snapshot/targets metadata with rollback,
  freeze and mix-and-match protection (TUF-style roles/thresholds);
- transparency log for publication metadata and provenance attestations;
- trusted publishing with short-lived OIDC credentials instead of long-lived
  upload tokens when supported;
- publisher-key/identity rotation and compromise recovery defined before use;
- staged publication, malware response, advisories and retraction metadata;
- registry mirrors cannot alter identity, digest or provenance policy.

Provenance is exposed to users and policy, but never presented as a “safe code”
badge. npm explicitly documents this limitation: provenance connects an
artifact to source/build context; consumers still have to decide whether to
trust the code.

#### Build-time and runtime authority

- Resolving, fetching, parsing and compiling a dependency does not execute its
  code. Arandu has no `preinstall`, `postinstall`, `build.rs` equivalent or
  programmable manifest in v0.1.
- Native libraries, proc-macro-like extensions and prebuilt binaries are not
  ordinary source dependencies. Each needs a future capability contract,
  explicit allowlist and sandbox/provenance policy.
- A source dependency is eventually linked into the application and can be
  malicious when the application runs. No package manager can hash this risk
  away. `arandu audit`, review, least-authority stdlib APIs and application
  sandboxing remain necessary.
- Dependency tests are not trusted build hooks and are not run automatically
  while consuming the package.

#### Verification and incident response

Planned user-visible tools:

```text
arandu tree                 exact direct/transitive graph and origins
arandu metadata             machine-readable package/target/module graph
arandu verify               lockfile, cached trees, signatures/provenance
arandu audit                advisories, yanks/retractions and policy violations
arandu update <package>     explicit resolution diff
arandu vendor               verified offline source snapshot
```

The lockfile remains buildable after a version is retracted, but `audit` and
updates warn or fail according to policy. A compromised version is never
silently replaced, because doing so would destroy reproducibility and could
substitute a different attack. Security response is an explicit reviewed
lockfile transition.

### VCS behavior is conservative

`arandu new` generates `.gitignore`, but does not require Git. Git initialization
is explicit (`--vcs=git`) or disabled (`--vcs=none`); in `auto`, an enclosing
repository is reused and a nested repository is never created. `arandu init`
operates on an existing directory and refuses to overwrite non-generated files.

Generated ignore entries initially cover:

```gitignore
/target/
/.arandu/
```

The lockfile is not ignored for applications/workspaces.

### Failures observed elsewhere that Arandu must avoid

| Failure class | Arandu guardrail |
| --- | --- |
| dependency install/build hooks execute arbitrary code | no executable manifest or lifecycle hook in v0.1 |
| offline mode silently resolves a different graph | `--offline` and `--locked` are independent; `--frozen` requires both |
| lockfile changes unexpectedly during ordinary commands | resolution policy is explicit and atomic; CI uses `--locked` |
| different origins produce duplicate logical packages | canonical source identity and collision diagnostics |
| mutable/shared cache contaminates builds | immutable content-addressed entries, digest verification and per-entry locks |
| global cache grows forever | inspect, verify and bounded prune commands; never implicit destructive cleanup |
| case-insensitive filesystems collapse distinct names | portable name validation and canonical lowercase control files |
| workspace child configuration shadows the root | one root lockfile and metadata directory; nested roots are diagnosed |
| path dependencies escape a sandbox or package boundary | normalized paths, explicit workspace membership and containment checks |
| failed build leaves a plausible partial artifact | staging and atomic publication of final outputs |
| arbitrary unknown manifest keys appear accepted | strict owned schema with a namespaced metadata escape hatch |
| consumers deep-import private source files | explicit target export map; file existence does not imply public API |
| transitive dependency accidentally becomes importable | only direct dependency aliases enter the module map |
| package rename rewrites every consumer import | manifest dependency key is the stable source-level alias |
| module resolution probes several filesystem layouts | one canonical logical mapping; no extension/index probing |
| source imports smuggle URLs, versions or host paths | dependency source belongs only to manifest/lockfile |
| package and module graphs invalidate each other wholesale | separate immutable inputs and tracked module edges |
| dependency confusion/private-name takeover | imports never discover packages; source/scopes are explicit and never fall back |
| tag or archive changes after resolution | exact commit plus independent normalized tree digest |
| malicious legitimate release | no automatic updates; provenance is not treated as safety; audit/review/policy remain required |
| compromised registry or mirror | signed versioned metadata, immutable digests and future transparency log |
| rollback/freeze/mix-and-match metadata attack | future registry must implement expiring TUF-style role metadata |
| install-time dependency code execution | no lifecycle hooks, executable manifests or build scripts in v0.1 |
| private package name leaks to public service | source routing is explicit; private origin has no public fallback |
| dependency graph/archive resource exhaustion | strict graph, archive, expanded-size and file-count limits |

### Implementation evidence

#### P0 — Contract and compatibility

- Make `arandu.toml` canonical and implement conflict-safe legacy discovery
  (`arandu.lock` becomes canonical when P3 introduces it).
- Replace the ad-hoc parser with a complete, deterministic TOML decoder.
- Add schema, derived package kind, edition and toolchain compatibility.
- Reject duplicates, unknown owned fields, invalid SemVer and unsafe paths.
- Keep manifest and directory data as narrow Salsa inputs.
- Reserve versioned capability-policy and compiler-produced effect-summary
  metadata without claiming A2 inference before it exists.

#### P1 — Project creation and VCS

- Implement `arandu init` and `new --bin/--lib`.
- Generate README, `.gitignore`, `src/` and `tests/` without partial trees.
- Add `--vcs=auto|git|none`, detecting enclosing repositories.
- Test reserved names, Unicode, spaces, case collisions and interrupted creation.

#### P2 — Artifact lifecycle

- Define profiles, target triples and `target/` layout.
- Make `build` produce a real stable artifact outside the monorepo.
- Publish through staging/atomic rename and retain the last valid artifact.
- Implement safe `arandu clean` and artifact provenance metadata.

#### P3 — Lockfile core

- Define a versioned deterministic lock format and canonical serializer.
- Implement atomic generation plus `--locked`, `--offline`, `--frozen`.
- Reject corruption, stale manifest fingerprints and nonportable fields.
- Prove byte-identical output across Windows, Linux and macOS.

#### P4 — Local packages and workspaces

- Introduce typed `PackageId`, `TargetId`, `ModuleId` and logical import roots.
- Support `bin` and `lib` targets plus relative path dependencies.
- Bind source imports through direct dependency aliases, `self` and `std`.
- Add explicit library export maps and reject dependency deep imports.
- Migrate bare local and quoted filesystem imports without ambiguous lookup.
- Add a single-root workspace with members and shared output/lockfile.
- Resolve a deterministic package DAG with cycle/collision diagnostics.
- Feed `PackageModuleMap` through Salsa inputs without filesystem reads in queries.
- Prove body-edit/export-surface/package-version invalidation boundaries.
- Refresh the live CLI/LSP package graph with cutoff after manifest edits.

The implemented P4 core resolves only workspace-contained relative path
dependencies. Discovery canonicalizes each package root, rejects escapes,
undeclared members, duplicate identity aliases and package cycles, then sorts
the graph before allocating checked typed IDs. The resulting lock records each
package's portable workspace-relative source and semantic manifest fingerprint;
no host path enters it.

The CLI registers source files before installing an authoritative
`PackageModuleMap` Salsa input. `self` bindings cover current-package modules;
only direct dependency aliases and explicit library exports become external
bindings. A registered private file or transitive package therefore remains
unresolvable even though its bytes are already known to the database. The
live LSP reload is coalesced as background work and commits the manifest,
directory roots and `PackageModuleMap` through stable Salsa input identities.
Invalid intermediate manifests preserve the last valid graph; successful
changes invalidate open importers without restarting the server. Package mode
also diagnoses quoted filesystem imports and emits a structured replacement
for legacy bare local imports. With these contracts covered, P4 is complete.

#### P5 — Verified global cache

- Specify platform-native cache/config locations and override flags.
- Store immutable content-addressed package sources with per-entry locking.
- Add `arandu cache inspect|verify|prune` with bounded, recoverable behavior.
- Never trust an extracted directory without rechecking its recorded digest.
- Bound archive bytes, expanded bytes, file count, graph depth and graph size.

P5-A fixes the cache root precedence as `--cache-dir`, then
`ARANDU_CACHE_DIR`, then the platform-native per-user cache: Local AppData on
Windows, `~/Library/Caches/Arandu` on macOS, and `$XDG_CACHE_HOME/arandu` (or
`~/.cache/arandu`) on other Unix systems. Explicit overrides must be absolute;
relative XDG values are ignored as required by the XDG Base Directory
specification. The versioned layout separates archives, extracted trees,
metadata, staging, quarantine and per-digest locks. Cache identities are
canonical SHA-256 digests and never embed mutable package names or origins.

P5-B publishes archive objects only after hashing the complete byte stream. A
native OS lock scoped to that digest covers revalidation and publication, so
unrelated packages never contend on a global download lock and process death
cannot leave an owned lock behind. Publication uses a uniquely named staging
file, flushes it before an atomic same-volume rename and never rewrites a valid
object. A same-address object with different bytes is quarantined and repaired;
staging files are never cache hits. Extracted trees remain untrusted until the
P5 verification step records and rechecks their deterministic tree digest.

P5-C exposes deterministic `cache inspect`, streaming `cache verify` and
transient-only `cache prune`. Every traversal is bounded by entry and byte
budgets (with explicit `--max-entries` and `--max-bytes` overrides), refuses
symlinks and malformed address paths as valid objects, and fails closed when a
budget is exhausted. `prune --dry-run` previews exactly the same candidate set;
normal prune removes only staging and quarantined files. Valid archives remain
untouched until remote lockfiles provide auditable garbage-collection roots.

P5-D adds `cache verify-tree <archive-digest> <tree-digest>`. It computes a
canonical SHA-256 over sorted portable paths, entry kinds, lengths and file
bytes, rejects symlinks and non-portable path components, and enforces
expanded-byte, file-count and depth limits. Package discovery now applies the
same fail-closed policy to graph nodes, dependency edges and traversal depth
(`PackageGraphLimits`), before assigning stable package identities. P5 therefore
does not infer graph size from archive bytes, and P6 can add remote-resolution
limits without weakening the local package contract.

#### P6 — Remote Git, intentionally narrow

- Accept secure Git/HTTPS sources pinned to an exact commit.
- Record canonical origin, commit and content digest in the lockfile.
- Disable network in `--offline`; never fall back from private to public origins.
- Make first trust and every update an explicit reviewable graph diff.
- Add `tree`, `verify`, `audit` and verified `vendor` foundations.
- Defer floating branches, arbitrary URLs, registries and dependency scripts.

P6-A freezes the remote manifest identity before networking is introduced:
`alias = { git = "https://host/owner/repository.git", rev = "<full-commit>" }`.
The origin must be canonical ASCII HTTPS, use a lowercase DNS host, contain no
credentials, custom port, query, fragment or path traversal, and end in
`.git`. `rev` accepts only a complete lowercase 40- or 64-hex commit object ID;
branch names, tags, abbreviated hashes and symbolic refs are invalid. `path`
and `git` are mutually exclusive because each dependency has exactly one
authority. The parser and regression suite reject every deferred source form;
supporting one later requires a new reviewed contract rather than accidentally
expanding this grammar.

P6-B upgrades the generated lock format to version 2 and separates Git
provenance into `origin`, `commit` and `content_digest`. The canonical
`source = "git+<origin>#<commit>"` must agree byte-for-byte with the explicit
fields, while `content_digest` is a lowercase SHA-256 identity for the verified
source tree. Root and path packages are forbidden from carrying remote
provenance. This makes origin substitution, commit substitution and content
tampering independently visible in review instead of hiding all authority in
one overloaded source string. Materialization will only publish a Git package
after it can populate all three fields.

P6-C materializes a remote through the system Git client only after the
manifest has fixed a canonical HTTPS origin and complete commit ID. The child
process is non-interactive, ignores system/global Git configuration, permits
only the HTTPS transport, disables hooks and fetches no tags. Arandu verifies
that `FETCH_HEAD^{commit}` is exactly the requested object, checks out without
submodules, rejects symlinks and special/non-portable entries, hashes the
bounded canonical tree and atomically publishes it under that SHA-256 identity.
An existing cache hit is rehashed before every use; corruption fails closed in
offline mode and an online refetch must still reproduce the digest in the lock.

P6-D connects verified Git roots to the same deterministic package graph,
module export map and generated lock used by local dependencies. Remote Git
dependencies may themselves depend on other exact-commit Git packages, graph
budgets and cycle detection still apply, `--locked` refuses an unrecorded
remote identity, and `--offline` cannot invoke Git. CLI project commands use
this path for fetch and reuse; the LSP uses the same `arandu_package` verifier
in locked, cache-only mode and therefore never adds analyzer-startup network
latency. Path subpackages contained inside a remote tree retain the enclosing
origin, commit and content digest while receiving a distinct portable source
identity. Neither frontend silently falls back to a different origin or an
unlocked graph.

P6-E starts with `arandu tree` and `arandu verify`. `tree` prints the canonical
graph fingerprint, sorted source identities, content digests and resolved
edges, giving code review a stable representation independent of cache paths.
`verify` forces `--locked --offline`, checks canonical lock bytes and rehashes
every referenced remote tree before reporting success. This is the shared
verification substrate used by `audit` and the atomic verified `vendor`.
`audit` reports locked origin, commit and content identity without claiming a
vulnerability scan when no advisory database is configured. `vendor` copies
only revalidated cache trees, verifies the copies and publishes a deterministic
content-named snapshot transactionally.

P6-F makes remote authority changes readonly by default. First trust, removal,
origin/commit changes, content changes and transitive edge changes produce a
stable line-oriented graph diff while preserving the previous lockfile.
`arandu update --accept` is the only ordinary command authorized to publish
that reviewed remote graph. Local workspace-only lock maintenance remains
automatic because it introduces no external code authority. LSP discovery stays
locked and offline and therefore cannot accept an update.

P7-B exercises independent OS processes on Windows and Unix rather than
assuming thread tests model filesystem behavior. Immutable cache objects retain
their per-digest lock, while project lockfile publication and each target
profile use persistent native lock inodes with a single acquisition order.
Staging is fully flushed before rename/replace; an interrupted staging file is
never an authoritative cache object, lockfile or build state, and the OS releases
the held native lock when the process exits. This is a process-crash contract,
not a claim that arbitrary filesystems survive power loss without directory
durability guarantees.

P7-C treats reproducibility as a semantic contract, not merely two executions
inside the same checkout. Equivalent projects are rebuilt in differently named
directories with varied timezone, locale, clock input and TOML declaration
order. Their graph output, canonical LF-only lockfile and `build-state.json`
must be byte-identical. Package and dependency-edge inputs are sorted before
fingerprinting/serialization. Capability and effect policy now participates in
the manifest fingerprint so an authority change cannot hide behind an unchanged
lock identity; set-like policy and workspace lists are sorted so presentation
order alone creates no churn. Third-party `[metadata]` remains deliberately
outside compiler semantics.

P7-D applies fail-closed path policy at every trust boundary. Archive members
must already be normalized, portable and unique under case-folding; Unix
symlinks, Windows reparse points/junctions and other special entries are not
accepted as package-tree content. Project entry roots are canonicalized and
containment-checked before source discovery, while cache publication refuses
link-like archive objects and revalidates trees before they become trusted.
These checks follow the conservative extraction posture documented by Python's
`tarfile` data filter and Windows' reparse-point guidance.

#### P7 — Gold promotion

- Native lifecycle E2E on Windows, Linux and macOS outside the checkout.
- Concurrent build/cache tests and crash-interrupted atomic-write recovery.
- Determinism campaign for manifests, graphs, lockfiles and artifact metadata.
- Adversarial path, symlink/junction, malformed archive and cache-tamper tests.
- Dependency-confusion, origin-substitution, rollback and graph/archive-bomb tests.
- Installed SDK smoke: `new → check → build → run → clean`.
- Documentation and migration guide complete; no known P0/P1 defect.

### Published command surface

```text
arandu new <path> [--bin|--lib] [--vcs=auto|git|none]
arandu init [--bin|--lib] [--vcs=auto|git|none]
arandu check [--locked|--offline|--frozen]
arandu build [--profile <name>] [--target <triple>] [--locked|--offline|--frozen]
arandu run [--profile <name>] [--target <triple>] [--locked|--offline|--frozen]
arandu clean
arandu tree
arandu verify
arandu audit
arandu vendor
arandu update --accept
arandu metadata
arandu cache inspect|verify|prune
```

`add/remove`, a registry, publishing and general dependency build scripts are
not in the first Gold slice. Exact-commit Git graph changes use the deliberately
narrow `update --accept` review flow; the schema reserves room for the other
features without pretending they already exist.

## PONTOS DE MELHORIA (O que não está no roadmap)

- A infraestrutura de empacotamento e instalação ainda usa scripts Python,
  Bash e PowerShell. Isso não afeta o usuário do SDK publicado, mas mantém uma
  dependência de desenvolvimento que deve migrar para `xtask` ou para a própria
  toolchain quando o bootstrap self-hosted estiver maduro.
- O cache verificado conhece arquivos Git imutáveis, mas ainda não possui um
  coletor de lixo baseado em raízes de lockfiles. O `prune` permanece
  conservador e remove somente material transitório/quarentenado.
- Políticas de capabilities/effects já participam da identidade do manifesto,
  porém a inferência do effect system pertence a A2. Hoje elas são declarações
  versionadas, não prova semântica produzida pelo compilador.
- O código de CLI concentra parsing de comandos de projeto em `main.rs`. A
  separação deve ocorrer por domínio quando a próxima superfície de comando
  exigir mudanças, preservando as funções puras de `arandu_package`.

Os limites abaixo são deliberados, não dívida escondida: não há dependência
por branch/tag flutuante, URL arbitrária, registry público, script de build de
dependência ou fallback entre origens privadas e públicas.

## Futuro e Próximos Passos

- Migrar tooling de release para uma implementação portável sem exigir Python
  do colaborador somente depois de existir um bootstrap estável para essa
  migração; até lá, os scripts continuam testados pela matriz nativa.
- Introduzir registry apenas com identidade tipada, metadados assinados,
  digests imutáveis, proteção contra rollback e transparência auditável.
- Integrar resumos de effects inferidos ao lockfile e ao diff de autoridade
  quando A2 entregar esse contrato semântico.
- Avaliar resolução SemVer mais ampla somente quando houver registry. A fonte
  Git continua exigindo commit exato por design.

### References

- [Cargo manifests](https://doc.rust-lang.org/cargo/reference/manifest.html),
  [workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html),
  [build cache](https://doc.rust-lang.org/cargo/reference/build-cache.html),
  [`cargo new`](https://doc.rust-lang.org/cargo/commands/cargo-new.html) and
  [locked/offline/frozen semantics](https://doc.rust-lang.org/cargo/commands/cargo-generate-lockfile.html)
- [Go Modules reference](https://go.dev/ref/mod): module identity, workspaces,
  immutable cache, checksums, private-origin behavior and portable path rules
- [Reproducible Builds variance guidance](https://reproducible-builds.org/docs/adding-build-variance/),
  [timestamps](https://reproducible-builds.org/docs/timestamps/) and
  [`SOURCE_DATE_EPOCH`](https://reproducible-builds.org/specs/source-date-epoch/):
  vary path, locale, timezone and clock inputs instead of proving only repeated
  execution in one ambient environment
- [XDG Base Directory specification](https://specifications.freedesktop.org/basedir-spec/latest/),
  [Apple File System Programming Guide](https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/MacOSXDirectories/MacOSXDirectories.html)
  and [Windows Known Folders](https://learn.microsoft.com/windows/win32/shell/knownfolderid):
  native per-user cache locations and regenerable-cache semantics
- [Rust `File` locking](https://doc.rust-lang.org/stable/std/fs/struct.File.html)
  and [Cargo cache locking](https://doc.rust-lang.org/stable/nightly-rustc/cargo/util/cache_lock/index.html):
  process-scoped native locks, concurrency modes and the global-lock contention
  avoided by Arandu's immutable per-digest publication
- [Git lockfile API](https://git-scm.com/docs/api-lockfile.html): writer
  exclusion plus staging-and-rename publication so readers see the old or new
  complete value, never a partially written value
- [SQLite atomic commit](https://www.sqlite.org/atomiccommit.html): explicit
  crash and power-loss boundaries, including why flushing one file alone is
  not a universal power-loss durability guarantee
- [Dart package layout](https://dart.dev/tools/pub/package-layout),
  [lockfile behavior](https://dart.dev/tools/pub/versioning) and
  [`--enforce-lockfile`](https://dart.dev/tools/pub/cmd/pub-get)
- [Zig build system](https://ziglang.org/learn/build-system/) and
  [`build.zig.zon` contract](https://github.com/ziglang/zig/blob/master/doc/build.zig.zon.md)
- [SwiftPM dependency resolution](https://github.com/swiftlang/swift-package-manager/blob/main/Sources/PackageManagerDocs/Documentation.docc/ResolvingPackageVersions.md)
  and [registry integrity model](https://github.com/swiftlang/swift-package-manager/blob/main/Documentation/PackageRegistry/PackageRegistryUsage.md)
- [Cargo build scripts](https://doc.rust-lang.org/cargo/reference/build-scripts.html),
  used here as a warning against ambient dependency code execution
- [Go module authentication and checksum transparency](https://go.dev/ref/mod#authenticating-modules),
  including its private-module fallback/privacy constraints
- [npm provenance](https://docs.npmjs.com/generating-provenance-statements/),
  [trusted publishing](https://docs.npmjs.com/trusted-publishers/) and
  [script policy](https://docs.npmjs.com/cli/update.html#ignore-scripts), plus
  the [versioned package-lock format and hidden-lock timestamp caveat](https://docs.npmjs.com/cli/configuring-npm/package-lock-json/)
- [Cargo source replacement](https://doc.rust-lang.org/cargo/reference/source-replacement.html)
- [The Update Framework](https://theupdateframework.io/), used as the future
  registry baseline for rollback, freeze and mix-and-match resistance
- [TOML 1.0 specification](https://toml.io/en/v1.0.0) and
  [PEP 518 format comparison](https://peps.python.org/pep-0518/#other-file-formats)
- [Python static project metadata contract](https://packaging.python.org/specifications/declaring-project-metadata/)
- [Go module/package identity](https://go.dev/ref/mod) and
  [file-scoped import bindings](https://go.dev/ref/spec)
- [Swift package products, targets and modules](https://docs.swift.org/swiftpm/documentation/packagemanagerdocs/introducingpackages/)
  plus [package/module visibility](https://docs.swift.org/swift-book/documentation/the-swift-programming-language/accesscontrol/)
- [Rust visibility and public re-exports](https://doc.rust-lang.org/reference/visibility-and-privacy.html)
- [Dart libraries and package imports](https://dart.dev/language/libraries)
- [Node package exports](https://nodejs.org/api/packages.html#package-entry-points),
  used for the explicit public-subpath boundary rather than its runtime loader
