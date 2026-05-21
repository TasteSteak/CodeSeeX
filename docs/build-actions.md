# Desktop Build Action

CodeSeeX uses `.github/workflows/build-desktop.yml` to build runnable desktop artifacts on GitHub Actions.

## When It Runs

The workflow runs in five situations:

- Pushes to `main`, so the default branch always has current artifacts.
- Pushes to `codex/**`, so contributor build branches can be tested in a fork before opening a pull request.
- Tag pushes matching `v*`, so release candidate builds can be produced from version tags.
- Pull requests targeting `main`, so maintainers can see whether the packaging flow still works.
- Manual `workflow_dispatch`, so a maintainer can run the packaging workflow on demand.

## Build Matrix

The workflow uses one matrix job with one entry per operating system:

- `Linux x64` runs on `ubuntu-latest` and executes `npm run dist:linux`.
- `macOS arm64` runs on `macos-14` and executes `npm run dist:mac`.
- `Windows x64` runs on `windows-latest` and executes `npm run dist:win`.

GitHub's hosted runner documentation currently lists `macos-14` as an arm64 macOS runner, so this workflow produces Apple Silicon macOS artifacts. Add a second macOS matrix entry on an Intel runner if an Intel build is required.

## Step-By-Step Setup

1. `actions/checkout@v6` checks out the repository source into the runner workspace.
2. `actions/setup-node@v6` installs Node.js 22 and enables npm cache reuse based on `package-lock.json`.
3. `npm ci` installs dependencies exactly from the lockfile for reproducible builds.
4. `npm run check` runs the repository's syntax checks before packaging.
5. `npm run dist:linux`, `npm run dist:mac`, or `npm run dist:win` runs according to the matrix entry.
6. `find dist -maxdepth 2 -type f -print | sort` prints the generated files into the workflow log for quick inspection.
7. `actions/upload-artifact@v6` uploads the generated packages from `dist` as workflow artifacts.

The workflow sets `CSC_IDENTITY_AUTO_DISCOVERY=false` so electron-builder does not try to discover local signing identities on CI runners. The produced artifacts are suitable for CI validation and manual testing, but production macOS distribution still needs a Developer ID certificate and notarization setup.

## Artifact Names

Uploaded artifact groups are named by platform:

- `CodeSeeX-linux-x64`
- `CodeSeeX-macos-arm64`
- `CodeSeeX-windows-x64`

Each group contains the installable packages and electron-builder metadata files generated for that platform.

## Testing In A Fork

To test this workflow from a fork, push a branch under `codex/**`:

```sh
git switch -c codex/cross-platform-build-actions
git push -u origin codex/cross-platform-build-actions
```

Then inspect the fork workflow run:

```sh
gh run list --repo Rain156/CodeSeeX --branch codex/cross-platform-build-actions --workflow build-desktop.yml
gh run watch --repo Rain156/CodeSeeX <run-id>
```

When all matrix jobs pass, open a pull request from the fork branch to `TasteSteak/CodeSeeX:main`.
