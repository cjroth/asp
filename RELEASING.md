# Releasing

One workflow ships everything: the desktop app, the Rust CLI, the TypeScript
SDK, the wasm packages, and the Obsidian plugin — all under a single version.

## Cut a release

1. Go to **Actions ▸ release ▸ Run workflow**.
2. Pick either:
   - an explicit **version** (`X.Y.Z`), or
   - a **bump** (`patch` / `minor` / `major`) — leave version blank.
3. (Optional) tick **publish_npm** to also push the SDK to npm (needs an
   `NPM_TOKEN` secret).
4. Run it.

That single run will:

1. bump every manifest to the chosen version (`scripts/bump-version.sh`),
2. commit `release: vX.Y.Z` to `main` and push an annotated tag `vX.Y.Z`,
3. open a **draft** GitHub Release,
4. build and attach, in parallel from the tagged commit:
   - **Desktop** (Tauri): macOS `.dmg` (universal), Windows `.msi` + `.exe`,
     Linux `.deb` + `.rpm`,
   - **CLI**: `asp` for `x86_64-linux`, `aarch64`/`x86_64` macOS, `x86_64`
     Windows (`.tar.gz` / `.zip`),
   - **SDK + wasm**: `asp-sdk-X.Y.Z.tgz` and `asp-wasm-X.Y.Z.tar.gz`
     (nodejs + web targets),
   - **Obsidian plugin**: `main.js` + `manifest.json`,
5. publish the release once all builds are green.

## Versioning

There is one shared version across the monorepo. The Obsidian plugin historically
ran ahead of the Cargo workspace, so `bump-version.sh` computes the current
version as `max(workspace, plugin)` and bumps from there — the unified version
can never regress the plugin (BRAT / the community store require it to be
strictly increasing). You can run the script locally to preview:

```bash
scripts/bump-version.sh patch    # prints the resolved version; edits manifests
git restore .                    # undo the preview
```

## Protocol version

Separate from the release version above, ASP has a wire-protocol number `PROTO`
(`crates/asp-core/src/wire.rs`). Peers refuse a mismatched proto at the `Hello`
handshake — an old node meeting a newer one gets a clear "peer speaks proto N,
upgrade" error, never silent corruption.

**Current: `PROTO = 4`** (bumped 3 → 4 for the git bridge, which adds the
`GitCommit` / `GitIngest` / `GitPlan` log kinds — an old peer can't decode them).
The 3 → 4 bump shipped as a **coordinated same-day upgrade** of the whole (small)
fleet: no two-step "understand-then-author" release, because the peers are few and
all operated by us. **When you bump `PROTO`, upgrade every peer you can't
coordinate with in the same window, or a bridge node's git rows will lock older
peers out at the handshake.** Revisit the two-step discipline (ship a release that
*understands* the new kinds before one that *authors* them) once vaults exist that
we don't operate. See `specs/git-bridge.md` §6.2.

## Security notes

- Every third-party action is pinned to a full commit SHA (the trailing
  `# vX.Y.Z` comment is a human hint only).
- The default workflow token is read-only; only the jobs that write the bump
  commit, the tag, or release assets request `contents: write`.
- Dispatch inputs are passed through `env`, never interpolated into shell
  bodies, to avoid script injection.

## Branch protection

`prepare` pushes the bump commit and tag straight to `main` with the built-in
`GITHUB_TOKEN`. If `main` is protected against direct pushes, either grant the
GitHub Actions bot a push bypass, or wire a PAT / GitHub App token (with bypass)
into the `prepare` checkout step.

Note: commits pushed with `GITHUB_TOKEN` do not trigger other workflows, so
`ci.yml` will not re-run on the release commit — the release's own build jobs
compile every component and act as the gate.
