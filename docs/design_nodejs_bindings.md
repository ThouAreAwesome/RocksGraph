# Design: Node.js bindings — `npm install rocksgraph`

Status: proposal.

## Problem

Node.js is the largest runtime where SQLite embedded patterns thrive — Electron apps,
Next.js backends, CLI tools, and edge functions. A Node.js binding gives RocksGraph
access to the npm ecosystem with no server process to manage.

## Architecture — same binary protocol, new FFI thin-layer

The compact binary protocol designed for Python (§1 of `design_python_bindings.md`)
reuses without change. The only new code is the FFI thin-layer (~30 lines Rust via
`napi-rs`) and the JS builder (~300 lines).

```
npm install rocksgraph
  → rocksgraph.node (native addon via napi-rs)
  → builder.ts   (pure JS traversal builder)
```

### §1: napi-rs FFI layer (`src/lib.rs` — ~30 lines)

```rust
use napi_derive::napi;

#[napi]
pub struct Graph {
    inner: rocksgraph::Graph,
}

#[napi]
impl Graph {
    #[napi(factory)]
    pub fn open(path: String) -> napi::Result<Self> {
        Ok(Self { inner: rocksgraph::Graph::open(path)? })
    }

    #[napi]
    pub fn read(&self) -> ReadSession { ... }

    #[napi]
    pub fn tx(&self) -> TxSession { ... }

    #[napi]
    pub fn close(&mut self) -> napi::Result<()> { ... }
}

#[napi]
pub struct ReadSession { inner: rocksgraph::ReadSession }

#[napi]
impl ReadSession {
    #[napi]
    pub fn execute(&mut self, bytes: Buffer, propKeys: Option<Vec<String>>) -> napi::Result<serde_json::Value> {
        // napi-rs's `serde-json` feature converts `serde_json::Value`
        directly to a JS object in memory — no text serialization, no JSON.parse.
        Same direct conversion as PyO3's `value_to_py`.
    }
}

#[napi]
pub struct TxSession { inner: Option<rocksgraph::TxSession> }

#[napi]
impl TxSession {
    #[napi]
    pub fn execute(&mut self, bytes: Buffer, propKeys: Option<Vec<String>>) -> napi::Result<serde_json::Value> { ... }
    #[napi]
    pub fn commit(&mut self) -> napi::Result<()> { ... }
    #[napi]
    pub fn rollback(&mut self) -> napi::Result<()> { ... }
}
```

Key differences from PyO3:
- `Buffer` replaces `&[u8]` (napi-rs auto-converts Node `Buffer` ↔ Rust `&[u8]`)
- Response encoded as JSON via `serde_json::Value` (napi-rs converts to JS object
  in memory — no text serialization, no `JSON.parse`)
- `#[napi(factory)]` for static constructors (maps to `new Graph(path)` in JS)
- `mut self` works correctly (napi-rs allows it, unlike PyO3)

> **FloatVector serialization watch (vector search, v0.2+):** When `Value::FloatVector`
> is included in the response, `serde_json::Value` serializes it as a JSON
> `Array` of numbers. Even though napi-rs avoids text round-tripping, the JS side
> still receives a generic `Array<number>` instead of a `Float32Array`. This means:
> (a) each element is boxed as a JS `number` (f64), doubling memory vs. f32; and
> (b) SIMD-friendly typed array operations are unavailable.
>
> **Required fix before shipping vector search bindings:** intercept
> `Value::FloatVector` in the napi-rs layer *before* `serde_json` conversion and
> return it as a `Buffer` (raw LE f32 bytes). The JS `_post_process` function then
> wraps the `Buffer` in the `Vector` class (see `design_vector_codec.md` §5b), which
> exposes `.data` as a `Float32Array` via `new Float32Array(buf.buffer)`. No
> `serde_json` involvement at all for the vector payload.

### §2: JS builder (`index.ts` — ~250 lines)

```typescript
// --- Encoding constants ---
const OP_V = 1;
const OP_OUT = 10;
const OP_HASPROPERTY = 20;
// ... 60 opcodes total, reused from Python _codec.py

const PRED_EQ = 0;
const PRED_GT = 1;
// ... 9 predicates

// --- Binary encoder ---
function encodeString(s: string): Buffer { ... }
function encodePredicate(p: P): Buffer { ... }
function encode(steps: Array<[number, any]>): Buffer { ... }

// --- Post-processing ---
interface Vertex { id: number; label: string; properties: Record<string, any> }
interface Edge { src: number; dst: number; label: string; rank: number; properties: Record<string, any> }
interface Property { key: string; value: any }

class VertexWrapper {
    constructor(private d: Vertex) {}
    get id() { return this.d.id }
    get label() { return this.d.label }
    // etc.
}

class EdgeWrapper { ... }
class PropertyWrapper { ... }

function postProcess(value: any): any {
    // Converts raw objects to Vertex/Edge/Property wrappers.
    // {src, dst, label}  -> EdgeWrapper
    // {id, label}        -> VertexWrapper
    // {key, value}       -> PropertyWrapper
    // {objects, labels}  -> Path (recurse into objects[])
    // other dict         -> plain map — MUST recurse into VALUES
    //                       (for group() map entries)
    // Array              -> recurse each element
    // scalar             -> pass through
}

// --- Builder ---
class Traversal {
    private session: ReadSession | TxSession | null;
    private steps: Array<[number, any]>;
    private propKeys: string[] | undefined;

    constructor(session: ReadSession | TxSession | null, steps = [], propKeys?: string[]) {
        this.session = session;
        this.steps = steps;
        this.propKeys = propKeys;
    }

    private clone(): Traversal { return new Traversal(this.session, [...this.steps], this.propKeys) }
    private add(opcode: number, args: any): Traversal { const t = this.clone(); t.steps.push([opcode, args]); return t }

    // Start steps
    V(...ids: number[]): Traversal { return this.add(OP_V, ids) }
    addV(label: string): Traversal { return this.add(OP_ADDV, label) }
    addE(label: string): Traversal { return this.add(OP_ADDE, label) }

    // Traversal steps
    out(...labels: string[]): Traversal { return this.add(OP_OUT, labels) }
    in_<T extends string>(...labels: T[]): Traversal { return this.add(OP_IN, labels) }
    has(key: string, value: any): Traversal { return this.add(OP_HASPROPERTY, [key, value]) }
    hasLabel(...labels: string[]): Traversal { return this.add(OP_HASLABEL, labels) }
    // ... 40+ methods, same as Python builder

    // Modulators
    withProperties(...keys: string[]): Traversal { ... }
    by(key: string, order?: "asc" | "desc"): Traversal { ... }

    // Terminals
    next(): any { const r = this.toArray(); return r[0] ?? null }
    toArray(): any[] { return postProcess(this.session!.execute(encode(this.steps), this.propKeys)) }
    iterate(): void { this.toArray() }
    toSet(): Set<any> { return new Set(this.toArray()) }
}
```

### §3: User experience

```typescript
import { Graph, P, __, Int64 } from "rocksgraph";

const g = new Graph("./data");

// Write
const tx = g.tx();
tx.V().addV("person").property("id", 1).property("name", "Alice").next();
tx.V().addE("knows").from_(1).to(2).next();
tx.commit();

// Read
const snap = g.read();
snap.V().V(1).out("knows").values("name").toArray();
// → ["Bob"]

snap.V().V().has("age", P.gt(Int64(30))).values("name").toArray();
// → ["Alice"]

snap.V().V().where(__.out("knows").hasId(2)).values("name").toArray();
// → ["Alice"]

g.close();
```

### §4: Workspace layout

```
bindings/nodejs/
  ├── src/
  │   └── lib.rs          # napi-rs FFI (~30 lines)
  ├── index.ts            # builder + encoder (~250 lines, at project root so index.js is the npm entry)
  ├── index.test.ts       # integration tests (vitest)
  ├── package.json        # "napi" build config
  ├── Cargo.toml          # deps: napi, napi-derive, rocksgraph, serde_json
  ├── build.rs            # napi-rs build script
  └── tsconfig.json
```

`Cargo.toml`:
```toml
[package]
name = "rocksgraph-node"
version = "0.1.1"
edition = "2021"
license = "MIT OR Apache-2.0"

[lib]
crate-type = ["cdylib"]

[dependencies]
napi = { version = "2", features = ["napi9", "serde-json"] }  # napi9 = Node.js 18+ minimum
napi-derive = "2"
rocksgraph = { version = "0.1.1", path = "../../rocksgraph" }
serde_json = "1"
```

`package.json`:
```json
{
  "name": "rocksgraph",
  "version": "0.1.1",
  "main": "index.js",
  "types": "index.d.ts",
  "napi": {
    "name": "rocksgraph",
    "triples": {
      "defaults": true
    }
  },
  "devDependencies": {
    "@napi-rs/cli": "^3",
    "vitest": "^2"
  },
  "scripts": {
    "build": "napi build --release",
    "test": "vitest"
  }
}
```

### §5: Cross-platform support

`@napi-rs/cli` handles cross-compilation for all targets via GitHub Actions:

```yaml
# .github/workflows/nodejs-release.yml
build:
  strategy:
    matrix:
      include:
        - { os: ubuntu-latest, target: x86_64-unknown-linux-gnu }
        - { os: macos-latest, target: aarch64-apple-darwin }
        - { os: macos-13, target: x86_64-apple-darwin }
        - { os: windows-latest, target: x86_64-pc-windows-msvc }
  steps:
    - uses: napi-rs/napi-action@v1
      with:
        target: ${{ matrix.target }}
        working-directory: bindings/nodejs
```

Or use the existing `PyO3/maturin-action` style — napi-rs has its own GitHub Action that builds and publishes to npm.

### §6: Implementation plan

| MR | Scope | Verify |
|----|-------|--------|
| 1 | napi-rs FFI layer (src/lib.rs) + Cargo/package config | `node -e "const {Graph} = require('.'); console.log(typeof Graph.open)"` works |
| 2 | JS builder + encoder (index.ts) | same builder pattern as Python, passes encode/decode round-trip |
| 3 | Integration tests (vitest) | full E2E: open → write → commit → read → traverse → close |
| 4 | GitHub Actions release workflow (napi-rs action) | publishes to npm |

### §7: Known limitations (postProcess)

Same caveats as Python:
- **`group()` without `by()`** uses integer vertex IDs as map keys (hashable).
- **`group()` map values** are recursively post-processed so inner Vertex/Edge
  dicts become wrappers.

### §8: Out of scope

- **Async sessions** — Synchronous embedded DB bindings are an established Node.js
  pattern (`better-sqlite3` is synchronous and has 2x the weekly downloads of
  `node-sqlite3`). Async wrappers are a community contribution.
- **Streaming cursors** — `toArray()` fetches all results. A `ReadableStream`
  wrapper is v0.2.
- **Deno / Bun** — support npm packages natively once published on npm.

### §9: Why napi-rs over alternatives

| Approach | Lines | Maintenance burden |
|----------|:-----:|:------------------:|
| napi-rs (Rust macros) | ~30 | same Rust binary protocol, same codec |
| node-ffi (pure JS) | ~50 | no Rust at all, but manual C ABI calling |
| WASM | ~200 | cross-platform but no filesystem access for RocksDB |
| Tauri plugin | ~100 | only for desktop apps, not general Node.js |

napi-rs wins: it's the Node.js equivalent of PyO3 — compile-once, native performance, and the binary protocol is already language-agnostic.
