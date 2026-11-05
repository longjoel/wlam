# WLAM Studio

A local browser workspace for importing a Windows application ZIP, selecting its
entry EXE, inspecting compatibility, queueing a recompile, and serving or
downloading the resulting browser bundle. Python 3.11+; no packages to install.

```sh
python3 apps/studio/server.py
# Open http://127.0.0.1:8787
```

The default analyzer is `target/debug/wlam`. Build `wlam-cli` using the root
README's toolchain instructions or set `WLAM_BIN` to an absolute binary path.
Analysis uses `wlam analyze EXE --json --no-oracle`; exit 2 is a completed
analysis with blocking imports. It does not use local Wine installations.

The default compiler adapter invokes `wlam recompile` and packages its
`guest.wasm`, `image.bin`, and version 1 manifest with a browser diagnostic
runner. The runner maps the PE image, seeds the stack/TIB/registers, and calls
the guest's entry in a Web Worker. It refuses every unimplemented browser Win32
API by name. No end-to-end Windows game support is claimed: graphics, audio,
filesystem mounting, and event scheduling require a browser host implementation.
The worker can be stopped; a 30-second watchdog terminates stalled runs.

## Compiler handoff

To replace the built-in adapter with a full browser runtime bundler, configure
a trusted, administrator-supplied command as a JSON argument array:

```sh
export WLAM_STUDIO_COMPILER='["/absolute/path/to/browser-bundler", "--input", "{input}", "--assets", "{assets}", "--output", "{output}"]'
python3 apps/studio/server.py
```

`bundle.py` is the default adapter and documents the current CLI format in code.
Set `WLAM_STUDIO_COMPILER=null` to disable builds while retaining analysis.
Custom adapters can package the result with the full browser Win32 host.

Arguments are expanded individually and executed without a shell:

| Placeholder | Meaning |
| --- | --- |
| `{input}` | Absolute path to selected EXE, including its original subdirectories |
| `{assets}` | Absolute directory containing the entire extracted archive |
| `{output}` | Fresh absolute directory for a self-contained browser bundle |

An adapter must exit 0 and emit `index.html`, guest wasm, JavaScript loader,
its browser host or diagnostic runner, and any assets needed at runtime. Use
relative URLs. `index.html` is mandatory; symlinks are rejected. Unsupported
host APIs must report errors rather than silently return fabricated results.
The worker passes assets to the adapter; copying and mounting them in the
guest filesystem belongs to the bundle's runtime. Native subprocesses analyze
and compile the PE; Studio never executes it as a native application.

Analysis blocks compilation when the analyzer reports blocking imports. A
clean import report is not proof that instructions, graphics, audio, or file
APIs work. Compiler output and exit failures appear in the UI. Compilation has
a five-minute timeout, a single worker, and a queue limit of eight projects.
Logs are shown after each subprocess completes (not live-streamed).

## Output and persistence

Projects, extracted files, metadata, and bundles live in `apps/studio/.data/`
(ignored by Git). `--data PATH` overrides this location. Metadata persists across
restarts; interrupted jobs become failed and can be retried. Rebuilding replaces
the project's previous output. There is no artifact version history yet.

Studio listens on loopback port 8787, with a read-only artifact server on 8788;
override using `--port` and `--player-port`. The browser preview is a sandboxed
iframe on that separate origin. Studio sends COOP/COEP headers and serves wasm
with `application/wasm`. Downloaded ZIPs can be deployed to a static host;
threaded runtimes need the same isolation headers on that host. The preview is
opt-in: a completed build does not automatically execute application code.

This is a local developer tool for applications you trust, consistent with the
repository's security model. It is not a public multi-user upload service or a
native process sandbox. Public deployment needs authenticated ownership,
per-job container/process resource limits, storage quotas and cleanup, TLS, and
an artifact origin on a separate site with no control-plane credentials.
Projects are not automatically published to the internet.

ZIP uploads are limited to 64 MiB, 256 MiB expanded, and 10,000 entries.
Traversal, absolute/Windows paths, duplicate paths, encrypted entries, and
symlinks are rejected. Uploaded archives are never used to choose commands.

## Can the compiler itself run in a browser?

Potentially. The pipeline would be `ZIP → worker → PE parser → Sleigh lifter →
wasm emitter → browser bundle`, with the compiler also compiled to wasm. Use
an in-memory input/output API; a Web Worker keeps compilation off the UI thread.
That is separate from executing the emitted guest wasm.

This has not been built or verified. The Sleigh dependencies need a target audit
and a wasm build; native filesystem, subprocess, threading, and timing calls
need removal or web adapters. Rust's `wasm32-unknown-unknown` target does not
provide native filesystem or OS thread operations:
[Rust target documentation](https://doc.rust-lang.org/stable/rustc/platform-support/wasm32-unknown-unknown.html).
The runtime also has to satisfy the project's 4 GiB memory contract and supply
real Win32 imports. A browser compiler does not solve missing Win32 host APIs.

The native worker path lets the studio track the evolving recompiler without
requiring that port first. The adapter boundary can later be replaced with a
browser-worker implementation while keeping the same project workflow.

## Validation

```sh
python3 -m unittest discover -s apps/studio/tests -v
node --test apps/studio/tests/loader.test.mjs
node --check apps/studio/static/app.js
# With the native CLI built:
python3 apps/studio/tests/native_smoke.py
```

Orchestration tests use an explicitly labeled compiler fixture. Loader tests
exercise real wasm imports and in-memory image loading. They cover hostile archive paths, build failures,
blockers, missing runtime files, restart recovery, bundle download, MIME types,
HTTP origin checks, and separation of API/source files from artifact hosting.
The native smoke check creates a real minimal PE32 executable, recompiles it
through the default adapter, loads the emitted bundle in Node's WebAssembly
engine, and verifies its x86 code stored 42 in guest memory. This validates the
bundle contract and wasm execution; it does not replace browser UI testing or
verify game support.
